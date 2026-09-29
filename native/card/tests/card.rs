use emmc_card::{
    Card, CardError, DEFAULT_CAPACITY_BLOCKS, MAX_TRANSFER_BYTES, RandomAccessRead,
    SMALL_CAPACITY_BLOCKS,
};
use periph::{
    edma::TcdSnapshot,
    esdhc::{self, DmaBuffers, Esdhc, transfer_dma59},
};

struct TinyBacking(Vec<u8>);

#[test]
fn card_media_flows_through_bounded_dma59_in_both_directions() {
    let media: Vec<u8> = (0..1024).map(|n| (n % 251) as u8).collect();
    let mut card =
        Card::with_backing(DEFAULT_CAPACITY_BLOCKS, Some(Box::new(TinyBacking(media)))).unwrap();
    let mut tcd = TcdSnapshot {
        saddr: 0x8000_0000,
        attr: 0,
        soff: 512,
        nbytes: 512,
        slast: -512,
        daddr: 0x8000_0000,
        citer: 1,
        doff: 0,
        dlast: 0,
        biter: 1,
        csr: 0,
    };
    let mut guest = [0u8; 512];
    let mut read_window = card.data_for(18, 1, 512).unwrap().unwrap();
    let read = transfer_dma59(
        18,
        tcd,
        &mut DmaBuffers {
            guest_base: 0x8000_0000,
            guest: &mut guest,
            card: &mut read_window,
        },
    )
    .unwrap();
    assert_eq!(guest.as_slice(), read_window.as_slice());
    assert_eq!(read.bytes, 512);
    assert_eq!(read.tcd.daddr, 0x8000_0200);

    // Card contents are only committed after the transfer succeeds. CMD25
    // reads the guest's own window and returns a bounded staging buffer.
    guest[0] = 0x5a;
    tcd.saddr = 0x8000_0000;
    let mut write_window = [0u8; 512];
    let written = transfer_dma59(
        25,
        tcd,
        &mut DmaBuffers {
            guest_base: 0x8000_0000,
            guest: &mut guest,
            card: &mut write_window,
        },
    )
    .unwrap();
    card.write_data(25, 1, &write_window).unwrap();
    assert_eq!(written.tcd.saddr, 0x8000_0000);
    assert_eq!(card.data_for(18, 1, 512).unwrap().unwrap()[0], 0x5a);
}

#[test]
fn card_port_drives_real_controller_identity_and_bus_test() {
    let mut controller = Esdhc::new(Card::default());
    assert!(controller.write(esdhc::BASE + esdhc::XFERTYP, 4, 0x0100_0000));
    assert_eq!(
        controller.read(esdhc::BASE + esdhc::CMDRSP0, 4),
        Some(0xc0ff_8080)
    );

    assert!(controller.write(esdhc::BASE + esdhc::XFERTYP, 4, 0x0200_0000));
    assert_eq!(
        controller.read(esdhc::BASE + esdhc::CMDRSP1, 4),
        Some(0x4530_0000)
    );
    assert_eq!(
        controller.read(esdhc::BASE + esdhc::CMDRSP2, 4),
        Some(0x3030_3447)
    );

    assert!(controller.write(esdhc::BASE + esdhc::DATPORT, 4, 0x5a));
    assert!(controller.write(esdhc::BASE + esdhc::XFERTYP, 4, 0x0e3a_0010));
    assert_eq!(
        controller.read(esdhc::BASE + esdhc::DATPORT, 4),
        Some(0xffff_ffa5)
    );
}

impl RandomAccessRead for TinyBacking {
    fn len(&self) -> u64 {
        self.0.len() as u64
    }

    fn read_at(&self, offset: u64, destination: &mut [u8]) -> usize {
        let source = self.0.get(offset as usize..).unwrap_or_default();
        let count = source.len().min(destination.len());
        destination[..count].copy_from_slice(&source[..count]);
        count
    }
}

#[test]
fn identity_commands_match_oracle() {
    let mut card = Card::default();
    assert_eq!(card.command(0, 0), [0; 4]);
    assert_eq!(card.command(1, 0), [0xc0ff_8080, 0, 0, 0]);
    assert_eq!(
        card.command(2, 0),
        [0, 0x4530_0000, 0x3030_3447, 0x0011_0000]
    );
    assert_eq!(card.command(9, 0), [0, 0xafc0_0380, 0x0000_0a03, 0]);
    assert_eq!(card.command(10, 0), card.command(2, 0));
    assert_eq!(card.command(3, 0x1234_ffff), [0x900, 0, 0, 0]);
    assert_eq!(card.rca(), 0x1234);
    assert_eq!(card.command(7, 0x1234_0000), [0x900, 0, 0, 0]);
    assert!(card.selected());
    assert_eq!(card.command(7, 0x9999_0000), [0x900, 0, 0, 0]);
    assert!(!card.selected());
    card.command(7, 0x1234_0000);
    card.command(0, 0);
    assert!(!card.selected());
    assert_eq!(card.command(42, 0), [0x900, 0, 0, 0]);
}

#[test]
fn cid_and_ext_csd_bytes_match_python_oracle_literals() {
    let mut card = Card::default();
    let cid_bytes = card.command(2, 0).map(u32::to_be_bytes).concat();
    assert_eq!(
        cid_bytes,
        [
            0x00, 0x00, 0x00, 0x00, 0x45, 0x30, 0x00, 0x00, 0x30, 0x30, 0x34, 0x47, 0x00, 0x11,
            0x00, 0x00,
        ]
    );

    let Some(ext) = card.data_for(8, 0, 512).expect("CMD8 is bounded") else {
        panic!("CMD8 must return EXT_CSD");
    };
    let mut expected = [0_u8; 512];
    expected[0x9c..0x9f].copy_from_slice(&[0x00, 0x01, 0xd8]);
    expected[0xaf] = 0x01;
    expected[0xb7] = 0x01;
    expected[0xb9] = 0x01;
    expected[0xd4..0xd8].copy_from_slice(&[0x00, 0x76, 0x00, 0x00]);
    expected[0xde] = 0x01;
    expected[0xe3] = 0x08;
    assert_eq!(ext, expected);

    let small = Card::new(SMALL_CAPACITY_BLOCKS).expect("small capacity is supported");
    let Some(small_ext) = small.data_for(8, 0, 512).expect("CMD8 is bounded") else {
        panic!("CMD8 must return EXT_CSD");
    };
    assert_eq!(small_ext[0x98], 0x01);
    assert_eq!(&small_ext[0xd4..0xd8], &[0x00, 0x00, 0x00, 0x00]);
}

#[test]
fn backed_reads_sparse_overlay_and_out_of_range_match_oracle() {
    let mut card = Card::with_backing(
        DEFAULT_CAPACITY_BLOCKS,
        Some(Box::new(TinyBacking(vec![1, 2, 3]))),
    )
    .expect("default capacity is supported");
    assert_eq!(
        card.data_for(18, 0, 5).expect("bounded CMD18"),
        Some(vec![1, 2, 3, 0, 0])
    );
    assert_eq!(
        card.data_for(18, 1, 4).expect("bounded CMD18"),
        Some(vec![0; 4])
    );

    card.write_data(25, 0, &[9, 8, 7, 6])
        .expect("bounded CMD25");
    card.write_data(25, 1, &[4]).expect("bounded CMD25");
    assert_eq!(card.overlay_len(), 5);
    assert_eq!(
        card.data_for(18, 0, 5).expect("bounded CMD18"),
        Some(vec![9, 8, 7, 6, 0])
    );

    let mut reusable = [0xff; 2];
    card.read_into(1, &mut reusable)
        .expect("bounded stream read");
    assert_eq!(reusable, [4, 0]);
    card.write_data(24, 0, &[0]).expect("non-CMD25 is ignored");
    assert_eq!(
        card.data_for(17, 0, 1).expect("bounded unknown command"),
        None
    );
}

#[test]
fn oversized_transfer_is_rejected_before_allocation() {
    let card = Card::default();
    assert_eq!(
        card.data_for(18, 0, MAX_TRANSFER_BYTES + 1),
        Err(CardError::TransferTooLarge {
            requested: MAX_TRANSFER_BYTES + 1,
            maximum: MAX_TRANSFER_BYTES,
        })
    );
}

#[test]
fn bus_test_and_capacity_rejection_match_oracle() {
    let card = Card::default();
    assert_eq!(card.read_word(14, 0x0000_005a), 0xffff_ffa5);
    assert_eq!(card.read_word(19, 0x5a), 0);
    assert!(matches!(
        Card::new(4),
        Err(CardError::UnsupportedCapacity(4))
    ));
}
