//! Firmware-free Board gate for channel-29 TX and channel-28 RX effects.
//! Real ready-state frames still require a source-checked active host import.

use std::{cell::RefCell, rc::Rc};

use coldfire::Bus;
use emmc_card::{Card, DEFAULT_CAPACITY_BLOCKS};
use machine::{Board, BoardWriteError, CompletionPolicy, SemaphoreAddresses};
use periph::{
    dspi::{self, Peer},
    edma,
};

const SRAM: u32 = 0x8000_0000;
const TX_SOURCE: u32 = SRAM + 0x1bc0;
const RX_DEST: u32 = SRAM + 0x1000;

struct RecordingPeer(Rc<RefCell<Vec<Vec<u8>>>>);

impl Peer for RecordingPeer {
    fn exchange(&mut self, tx: &[u8]) -> Vec<u8> {
        self.0.borrow_mut().push(tx.to_vec());
        vec![0; tx.len()]
    }
}

fn board() -> Board {
    Board::new(
        Card::new(DEFAULT_CAPACITY_BLOCKS).unwrap(),
        SemaphoreAddresses::default(),
        CompletionPolicy::Oracle,
    )
}

fn tcd(board: &mut Board, chan: usize, source: bool) {
    let base = edma::TCD_BASE + (chan * 0x20) as u32;
    board
        .write16(base + edma::ATTR as u32, if source { 2 } else { 0 })
        .unwrap();
    board
        .write32(base + edma::NBYTES as u32, if source { 4 } else { 2 })
        .unwrap();
    board.write16(base + edma::CITER as u32, 2).unwrap();
    board.write16(base + edma::BITER as u32, 2).unwrap();
    if source {
        board.write32(base + edma::SADDR as u32, TX_SOURCE).unwrap();
        board.write16(base + edma::SOFF as u32, 4).unwrap();
    } else {
        board.write32(base + edma::DADDR as u32, RX_DEST).unwrap();
        board.write16(base + edma::DOFF as u32, 1).unwrap();
    }
}

#[test]
fn tx_serq_reads_board_ram_and_rx_serq_delivers_only_when_peer_accepts() {
    let mut board = board();
    board.map_ram_page(SRAM).unwrap();
    tcd(&mut board, dspi::TX_CHAN, true);
    tcd(&mut board, dspi::RX_CHAN, false);
    for (i, value) in [0x80, 0x01, 0x12, 0x34, 0x80, 0x01, 0x56, 0x78]
        .into_iter()
        .enumerate()
    {
        board.write8(TX_SOURCE + i as u32, value).unwrap();
    }
    for i in 0..4 {
        board.write8(RX_DEST + i, 0xff).unwrap();
    }
    let accepted = Rc::new(RefCell::new(Vec::new()));
    board.dma.peer = Box::new(RecordingPeer(Rc::clone(&accepted)));

    board.write8(edma::SERQ, dspi::TX_CHAN as u8).unwrap();
    assert!(
        accepted.borrow().is_empty(),
        "capture alone did not exchange"
    );
    board.write8(edma::SERQ, dspi::RX_CHAN as u8).unwrap();
    assert_eq!(*accepted.borrow(), vec![vec![0x12, 0x34, 0x56, 0x78]]);
    assert_eq!(board.dma.dspi2.frames, 1);
    assert_eq!(board.dma.dspi2.tx_bytes, 4);
    for i in 0..4 {
        assert_eq!(board.read8(RX_DEST + i).unwrap(), 0);
    }
    assert_eq!(board.take_dma_written_ranges(), vec![(RX_DEST, 4)]);
    assert!(board.take_dma_written_ranges().is_empty());
}

#[test]
fn dma_register_slots_are_restored_from_their_snapshot_page() {
    let mut board = board();
    let mut page = vec![0; 1024 * 1024];
    let offset = (edma::TCD_BASE - 0xfc00_0000) as usize + dspi::TX_CHAN * 0x20;
    page[offset + edma::CITER..offset + edma::CITER + 2].copy_from_slice(&2u16.to_be_bytes());
    board.import_ram_page(0xfc00_0000, &page).unwrap();
    assert_eq!(
        board.read16(edma::TCD_BASE + (dspi::TX_CHAN * 0x20 + edma::CITER) as u32),
        Ok(2)
    );
    assert_eq!(
        edma::TcdView::new(&mut board.dma.edma_regs, dspi::TX_CHAN).citer(),
        2
    );

    let mut page = vec![0; 1024 * 1024];
    let offset = (dspi::DSPI2_MCR - 0xec00_0000) as usize;
    page[offset..offset + 4].copy_from_slice(&0x1234_5678u32.to_be_bytes());
    board.import_ram_page(0xec00_0000, &page).unwrap();
    assert_eq!(board.read32(dspi::DSPI2_MCR), Ok(0x1234_5678));
}

#[test]
fn missing_dma_source_is_an_error_not_a_synthetic_zero_frame() {
    let mut board = board();
    tcd(&mut board, dspi::TX_CHAN, true);
    assert!(matches!(
        board.write_guest(edma::SERQ, 1, dspi::TX_CHAN as u32),
        Err(BoardWriteError::DspiDma {
            address: TX_SOURCE,
            ..
        })
    ));
    assert_eq!(board.dma.dspi2.frames, 0);
    assert_eq!(board.dma.dspi2.tx_bytes, 0);
}

#[test]
fn rx_first_and_set_all_serq_exchange_once_after_source_capture() {
    for serqs in [&[dspi::RX_CHAN as u8, dspi::TX_CHAN as u8][..], &[0x40][..]] {
        let mut board = board();
        board.map_ram_page(SRAM).unwrap();
        tcd(&mut board, dspi::TX_CHAN, true);
        tcd(&mut board, dspi::RX_CHAN, false);
        for (i, value) in [0x80, 0x01, 0x12, 0x34, 0x80, 0x01, 0x56, 0x78]
            .into_iter()
            .enumerate()
        {
            board.write8(TX_SOURCE + i as u32, value).unwrap();
        }
        let accepted = Rc::new(RefCell::new(Vec::new()));
        board.dma.peer = Box::new(RecordingPeer(Rc::clone(&accepted)));
        for &serq in serqs {
            board.write8(edma::SERQ, serq).unwrap();
        }
        assert_eq!(*accepted.borrow(), vec![vec![0x12, 0x34, 0x56, 0x78]]);
        assert_eq!(board.dma.dspi2.frames, 1);
        assert_eq!(board.take_dma_written_ranges(), vec![(RX_DEST, 4)]);
    }
}

#[test]
fn unmapped_rx_or_mismatched_length_rejects_before_peer_accepts() {
    for unmapped in [true, false] {
        let mut board = board();
        board.map_ram_page(SRAM).unwrap();
        tcd(&mut board, dspi::TX_CHAN, true);
        tcd(&mut board, dspi::RX_CHAN, false);
        if unmapped {
            let dest = SRAM + 0x10_1000;
            board
                .write32(
                    edma::TCD_BASE + (dspi::RX_CHAN * 0x20 + edma::DADDR) as u32,
                    dest,
                )
                .unwrap();
        } else {
            board
                .write32(
                    edma::TCD_BASE + (dspi::RX_CHAN * 0x20 + edma::NBYTES) as u32,
                    3,
                )
                .unwrap();
        }
        let accepted = Rc::new(RefCell::new(Vec::new()));
        board.dma.peer = Box::new(RecordingPeer(Rc::clone(&accepted)));
        board.write8(edma::SERQ, dspi::TX_CHAN as u8).unwrap();
        assert!(matches!(
            board.write_guest(edma::SERQ, 1, dspi::RX_CHAN as u32),
            Err(BoardWriteError::DspiDma { .. })
        ));
        assert!(accepted.borrow().is_empty());
        assert_eq!(board.dma.dspi2.frames, 0);
        assert_eq!(board.dma.dspi2.pending_tx_len(), Some(4));
    }
}

#[test]
fn zero_count_tx_against_nonempty_rx_is_rejected_before_exchange() {
    let mut board = board();
    board.map_ram_page(SRAM).unwrap();
    tcd(&mut board, dspi::TX_CHAN, true);
    tcd(&mut board, dspi::RX_CHAN, false);
    board
        .write16(
            edma::TCD_BASE + (dspi::TX_CHAN * 0x20 + edma::CITER) as u32,
            0,
        )
        .unwrap();
    let accepted = Rc::new(RefCell::new(Vec::new()));
    board.dma.peer = Box::new(RecordingPeer(Rc::clone(&accepted)));
    board.write8(edma::SERQ, dspi::RX_CHAN as u8).unwrap();
    assert!(matches!(
        board.write_guest(edma::SERQ, 1, dspi::TX_CHAN as u32),
        Err(BoardWriteError::DspiDma { .. })
    ));
    assert!(accepted.borrow().is_empty());
    assert_eq!(board.dma.dspi2.frames, 0);
}

#[test]
fn board_reads_tx_elements_across_smod_wrap_in_guest_ram() {
    let mut board = board();
    board.map_ram_page(SRAM).unwrap();
    tcd(&mut board, dspi::TX_CHAN, true);
    tcd(&mut board, dspi::RX_CHAN, false);
    let tx_base = edma::TCD_BASE + (dspi::TX_CHAN * 0x20) as u32;
    board.write16(tx_base + edma::ATTR as u32, 0x4002).unwrap(); // SMOD=8, elem=4
    board
        .write32(tx_base + edma::SADDR as u32, SRAM + 0x1bfc)
        .unwrap();
    for (addr, bytes) in [
        (SRAM + 0x1bfc, [0x80, 0x01, 0x12, 0x34]),
        (SRAM + 0x1b00, [0x80, 0x01, 0x56, 0x78]),
    ] {
        for (i, byte) in bytes.into_iter().enumerate() {
            board.write8(addr + i as u32, byte).unwrap();
        }
    }
    let accepted = Rc::new(RefCell::new(Vec::new()));
    board.dma.peer = Box::new(RecordingPeer(Rc::clone(&accepted)));
    board.write8(edma::SERQ, dspi::TX_CHAN as u8).unwrap();
    board.write8(edma::SERQ, dspi::RX_CHAN as u8).unwrap();
    assert_eq!(*accepted.borrow(), vec![vec![0x12, 0x34, 0x56, 0x78]]);
}
