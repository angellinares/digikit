use coldfire::Bus;
use emmc_card::{Card, DEFAULT_CAPACITY_BLOCKS};
use machine::{Board, CompletionPolicy, SemaphoreAddresses, Time, TimerPolicy};
use periph::{edma, ssi};

const RAM: u32 = 0x8000_0000;

fn board() -> Board {
    Board::new(
        Card::new(DEFAULT_CAPACITY_BLOCKS).unwrap(),
        SemaphoreAddresses::default(),
        CompletionPolicy::Oracle,
    )
}

fn tcd(board: &mut Board, chan: usize, saddr: u32, daddr: u32, csr: u16, dlast: u32) {
    let base = edma::TCD_BASE + (chan * 0x20) as u32;
    board.write16(base + edma::ATTR as u32, 0x0202).unwrap();
    board.write32(base + edma::NBYTES as u32, 4).unwrap();
    board.write32(base + edma::SADDR as u32, saddr).unwrap();
    board.write32(base + edma::DADDR as u32, daddr).unwrap();
    board.write16(base + edma::CITER as u32, 1).unwrap();
    board.write16(base + edma::BITER as u32, 1).unwrap();
    board
        .write16(
            base + edma::SOFF as u32,
            if chan == ssi::RX_CHAN { 0 } else { 4 },
        )
        .unwrap();
    board
        .write16(
            base + edma::DOFF as u32,
            if chan == ssi::RX_CHAN { 4 } else { 0 },
        )
        .unwrap();
    board.write32(base + edma::DLAST as u32, dlast).unwrap();
    board.write16(base + edma::CSR as u32, csr).unwrap();
}

#[test]
fn diagnostic_ssi_writes_marker_reloads_sg_and_reasserts_after_cint() {
    let mut board = board();
    board.map_ram_page(RAM).unwrap();
    let rx = RAM + 0x100;
    let tx = RAM + 0x200;
    let next = RAM + 0x300;
    tcd(&mut board, ssi::RX_CHAN, 0, rx, edma::CSR_E_SG, next);
    tcd(&mut board, ssi::TX_CHAN, tx, 0, edma::CSR_INT_MAJOR, 0);
    // A guest-installed descriptor, copied only on RX major completion.
    let mut image = [0u8; 0x20];
    image[0x10..0x14].copy_from_slice(&(RAM + 0x180).to_be_bytes());
    image[0x14..0x16].copy_from_slice(&1u16.to_be_bytes());
    image[0x1c..0x1e].copy_from_slice(&1u16.to_be_bytes());
    for (i, byte) in image.into_iter().enumerate() {
        board.write8(next + i as u32, byte).unwrap();
    }
    board.enable_ssi_diagnostic(ssi::AUDIO_SSI0_REQUEST_HZ, 132_000_000);
    board.write8(edma::SERQ, ssi::RX_CHAN as u8).unwrap();
    board.write8(edma::SERQ, ssi::TX_CHAN as u8).unwrap();
    let due = board.ssi_deadline(0).unwrap();
    let writes = board.service_ssi(due);
    assert_eq!(writes, vec![(rx, 4)]);
    assert_eq!(board.read32(rx), Ok(RxHandoverPeer::MARKER));
    assert_eq!(
        board.read32(edma::TCD_BASE + (ssi::RX_CHAN * 0x20 + edma::DADDR) as u32),
        Ok(RAM + 0x180)
    );
    assert!(board.dma.ssi.as_ref().unwrap().vector170_owed());
    board.write8(edma::CERQ, ssi::RX_CHAN as u8).unwrap();
    board.write8(edma::CINT, ssi::TX_CHAN as u8).unwrap();
    assert!(!board.dma.ssi.as_ref().unwrap().vector170_owed());
    let next_due = board.ssi_deadline(due).unwrap();
    board.service_ssi(next_due);
    assert!(board.dma.ssi.as_ref().unwrap().vector170_owed());
}

#[test]
fn diagnostic_ssi_observes_actual_intfrch1_value() {
    let mut board = board();
    board.attach_time(Time::new(TimerPolicy::Oracle, vec![], 132_000_000.0));
    board.enable_ssi_diagnostic(ssi::AUDIO_SSI0_REQUEST_HZ, 132_000_000);
    board
        .write32(ssi::INTFRCH1, ssi::INTFRCH1_SOURCE63)
        .unwrap();
    assert!(board.dma.ssi.as_ref().unwrap().vector191_owed());
    board.write32(ssi::INTFRCH1, 0).unwrap();
    assert!(!board.dma.ssi.as_ref().unwrap().vector191_owed());
}

#[test]
fn invalid_sg_and_unsupported_stride_leave_guest_memory_and_tcd_unchanged() {
    for invalid_sg in [true, false] {
        let mut board = board();
        board.map_ram_page(RAM).unwrap();
        let rx = RAM + 0x100;
        tcd(
            &mut board,
            ssi::RX_CHAN,
            0,
            rx,
            if invalid_sg { edma::CSR_E_SG } else { 0 },
            if invalid_sg { RAM + 1 } else { 0 },
        );
        if !invalid_sg {
            board
                .write16(
                    edma::TCD_BASE + (ssi::RX_CHAN * 0x20 + edma::DOFF) as u32,
                    2,
                )
                .unwrap();
        }
        board.write32(rx, 0xfeed_beef).unwrap();
        board.enable_ssi_diagnostic(ssi::AUDIO_SSI0_REQUEST_HZ, 132_000_000);
        board.write8(edma::SERQ, ssi::RX_CHAN as u8).unwrap();
        let due = board.ssi_deadline(0).unwrap();
        assert!(board.service_ssi(due).is_empty());
        assert_eq!(board.read32(rx), Ok(0xfeed_beef));
        assert_eq!(
            board.read32(edma::TCD_BASE + (ssi::RX_CHAN * 0x20 + edma::DADDR) as u32),
            Ok(rx)
        );
        assert_eq!(board.dma.ssi.as_ref().unwrap().rejected_requests, 1);
    }
}

#[test]
fn cint_nop_does_not_clear_and_dreq_disables_after_major_completion() {
    let mut board = board();
    board.map_ram_page(RAM).unwrap();
    tcd(
        &mut board,
        ssi::TX_CHAN,
        RAM + 0x200,
        0,
        edma::CSR_INT_MAJOR | edma::CSR_D_REQ,
        0,
    );
    board.enable_ssi_diagnostic(ssi::AUDIO_SSI0_REQUEST_HZ, 132_000_000);
    board.write8(edma::SERQ, ssi::TX_CHAN as u8).unwrap();
    let due = board.ssi_deadline(0).unwrap();
    board.service_ssi(due);
    assert!(board.dma.ssi.as_ref().unwrap().vector170_owed());
    assert!(!board.dma.ssi.as_ref().unwrap().enabled[ssi::TX_CHAN]);
    board.write8(edma::CINT, 0x80 | ssi::TX_CHAN as u8).unwrap();
    assert!(board.dma.ssi.as_ref().unwrap().vector170_owed());
}

use periph::ssi::RxHandoverPeer;

#[test]
fn nonbyte_serq_does_not_arm_ssi() {
    let mut board = board();
    board.enable_ssi_diagnostic(ssi::AUDIO_SSI0_REQUEST_HZ, 132_000_000);
    for channel in [ssi::RX_CHAN, ssi::TX_CHAN] {
        board.write16(edma::SERQ, channel as u16).unwrap();
        board.write32(edma::SERQ, channel as u32).unwrap();
        assert!(!board.dma.ssi.as_ref().unwrap().enabled[channel]);
        assert_eq!(board.ssi_deadline(0), None);
    }
}
