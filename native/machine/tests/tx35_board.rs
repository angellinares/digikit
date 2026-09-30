use coldfire::Bus;
use emmc_card::{Card, DEFAULT_CAPACITY_BLOCKS};
use machine::{Board, BoardWriteError, CompletionPolicy, SemaphoreAddresses};
use periph::edma;

const SRAM: u32 = 0x8000_0000;
const TX35: usize = 35;

fn board() -> Board {
    Board::new(
        Card::new(DEFAULT_CAPACITY_BLOCKS).unwrap(),
        SemaphoreAddresses::default(),
        CompletionPolicy::Oracle,
    )
}

fn tcd_base() -> u32 {
    edma::TCD_BASE + (TX35 * 0x20) as u32
}

fn configure(board: &mut Board, source: u32, attr: u16, soff: u16, nbytes: u32, citer: u16) {
    let base = tcd_base();
    board.write32(base + edma::SADDR as u32, source).unwrap();
    board.write16(base + edma::ATTR as u32, attr).unwrap();
    board.write16(base + edma::SOFF as u32, soff).unwrap();
    board.write32(base + edma::NBYTES as u32, nbytes).unwrap();
    board.write16(base + edma::CITER as u32, citer).unwrap();
    board.write16(base + edma::BITER as u32, citer).unwrap();
}

#[test]
fn tx35_drains_per_byte_soff_and_smod_source_ring() {
    let mut board = board();
    board.map_ram_page(SRAM).unwrap();
    configure(&mut board, SRAM + 0x1fc, 0x4000, 2, 2, 3); // SMOD=8
    for (offset, byte) in [
        (0x1fc, 1),
        (0x1fe, 2),
        (0x100, 3),
        (0x102, 4),
        (0x104, 5),
        (0x106, 6),
    ] {
        board.write8(SRAM + offset, byte).unwrap();
    }

    board.write8(edma::SERQ, TX35 as u8).unwrap();

    assert_eq!(board.take_uart_tx(), vec![1, 2, 3, 4, 5, 6]);
    assert!(board.take_uart_tx().is_empty());
    assert_eq!(board.dma.tx35.bytes, 6);
    assert_eq!(board.dma.tx35.transfers, 1);
    assert_eq!(board.dma.tx35.pending, 1);
    assert_eq!(board.read16(tcd_base() + edma::CITER as u32), Ok(3));
    assert_eq!(
        board.read32(tcd_base() + edma::SADDR as u32),
        Ok(SRAM + 0x108)
    );
    assert!(board.consume_tx35_pending());
    assert!(!board.consume_tx35_pending());
}

#[test]
fn tx35_zero_citer_is_a_noop_without_source_mapping() {
    let mut board = board();
    configure(&mut board, SRAM, 0, 1, 1, 0);

    board.write8(edma::SERQ, TX35 as u8).unwrap();

    assert!(board.take_uart_tx().is_empty());
    assert_eq!(board.dma.tx35.bytes, 0);
    assert_eq!(board.dma.tx35.transfers, 0);
    assert_eq!(board.dma.tx35.pending, 0);
}

#[test]
fn tx35_zero_nbytes_keeps_the_major_loop_completion_semantics() {
    let mut board = board();
    configure(&mut board, SRAM, 0, 1, 0, 1);

    board.write8(edma::SERQ, TX35 as u8).unwrap();

    assert!(board.take_uart_tx().is_empty());
    assert_eq!(board.dma.tx35.bytes, 0);
    assert_eq!(board.dma.tx35.transfers, 1);
    assert_eq!(board.dma.tx35.pending, 1);
    assert_eq!(board.read16(tcd_base() + edma::CITER as u32), Ok(1));
}

#[test]
fn tx35_unmapped_source_rejects_before_tcd_or_output_mutation() {
    let mut board = board();
    configure(&mut board, SRAM, 0, 1, 1, 2);

    assert!(matches!(
        board.write_guest(edma::SERQ, 1, TX35 as u32),
        Err(BoardWriteError::DspiDma {
            address: SRAM,
            bytes: 2
        })
    ));
    assert_eq!(board.read16(tcd_base() + edma::CITER as u32), Ok(2));
    assert!(board.take_uart_tx().is_empty());
    assert_eq!(board.dma.tx35.bytes, 0);
    assert_eq!(board.dma.tx35.transfers, 0);
    assert_eq!(board.dma.tx35.pending, 0);
}

#[test]
fn tx35_rejects_address_overflow_and_oversized_descriptor_before_mutation() {
    let mut board = board();
    board.map_ram_page(0).unwrap();
    configure(&mut board, 0, 0, u16::MAX, 1, 2);
    board.write8(0, 0xaa).unwrap();
    assert!(matches!(
        board.write_guest(edma::SERQ, 1, TX35 as u32),
        Err(BoardWriteError::DspiDma {
            address: 0,
            bytes: 2
        })
    ));
    assert_eq!(board.read16(tcd_base() + edma::CITER as u32), Ok(2));
    assert!(board.take_uart_tx().is_empty());

    configure(&mut board, SRAM, 0, 1, 4097, 1);
    assert!(matches!(
        board.write_guest(edma::SERQ, 1, TX35 as u32),
        Err(BoardWriteError::DspiDma {
            address: SRAM,
            bytes: 4097
        })
    ));
    assert_eq!(board.read16(tcd_base() + edma::CITER as u32), Ok(1));
    assert_eq!(board.dma.tx35.transfers, 0);
}
