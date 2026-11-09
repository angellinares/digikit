//! eDMA channel 37: the DN2 MIDI DIN transmit ring drained into the UART9 data register (finding 25).

use coldfire::Bus;
use emmc_card::{Card, CardIdentity};
use machine::{Board, CompletionPolicy, MIDI_TX_CHAN, MIDI_UDR, SemaphoreAddresses};
use periph::edma;

const SRAM: u32 = 0x8000_0000;

fn board() -> Board {
    Board::new(
        Card::new(CardIdentity::Mlc4G).unwrap(),
        SemaphoreAddresses::default(),
        CompletionPolicy::Oracle,
    )
}

fn tcd(field: usize) -> u32 {
    edma::TCD_BASE + (MIDI_TX_CHAN * 0x20) as u32 + field as u32
}

/// The DN2 1.11 driver's descriptor: a 4 KiB source ring (SMOD 12), one byte per minor loop, to `daddr`.
fn configure(board: &mut Board, source: u32, daddr: u32, citer: u16) {
    board.write32(tcd(edma::SADDR), source).unwrap();
    board.write16(tcd(edma::ATTR), 0x6000).unwrap();
    board.write16(tcd(edma::SOFF), 1).unwrap();
    board.write32(tcd(edma::NBYTES), 1).unwrap();
    board.write32(tcd(edma::DADDR), daddr).unwrap();
    board.write16(tcd(edma::CITER), citer).unwrap();
    board.write16(tcd(edma::BITER), citer).unwrap();
}

#[test]
fn a_serq_moves_the_ring_bytes_to_the_midi_output_and_wraps_the_source() {
    let mut board = board();
    board.map_ram_page(SRAM).unwrap();
    for i in 0..6u32 {
        board.write8(SRAM + 0xffa + i, 0x90 + i as u8).unwrap();
        board.write8(SRAM + i, 0xa0 + i as u8).unwrap();
    }
    configure(&mut board, SRAM + 0xffa, MIDI_UDR, 12);

    board.write8(edma::SERQ, MIDI_TX_CHAN as u8).unwrap();

    assert!(board.has_host_events());
    assert_eq!(
        board.take_midi_tx(),
        [
            0x90, 0x91, 0x92, 0x93, 0x94, 0x95, 0xa0, 0xa1, 0xa2, 0xa3, 0xa4, 0xa5
        ]
    );
    assert!(
        board.snap_save(&mut periph::snap::Writer::new()).is_err(),
        "a started transfer the host has not taken blocks a snapshot"
    );
    assert_eq!(board.take_midi_tx_started(), 12);
    assert_eq!(board.take_midi_tx_started(), 0);
    assert!(!board.has_host_events());
    assert_eq!(board.read32(tcd(edma::SADDR)), Ok(SRAM + 6));
    assert_eq!(board.read16(tcd(edma::CITER)), Ok(12));
}

#[test]
fn another_destination_or_an_empty_loop_moves_nothing() {
    let mut board = board();
    board.map_ram_page(SRAM).unwrap();
    configure(&mut board, SRAM, SRAM + 0x100, 4);
    board.write8(edma::SERQ, MIDI_TX_CHAN as u8).unwrap();
    assert!(board.take_midi_tx().is_empty());
    assert_eq!(board.take_midi_tx_started(), 0);
    assert_eq!(board.read32(tcd(edma::SADDR)), Ok(SRAM));

    configure(&mut board, SRAM, MIDI_UDR, 0);
    board.write8(edma::SERQ, MIDI_TX_CHAN as u8).unwrap();
    assert!(board.take_midi_tx().is_empty());
    assert_eq!(board.take_midi_tx_started(), 0);
}
