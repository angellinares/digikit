//! Unit tests against MCF5441XRM chapter 40 (DSPI) and `emu/dspi2.py`'s
//! `Dspi2Link`, independent of any trace.

use periph::dspi::{self, Dspi2Link, Peer, RX_CHAN, TX_CHAN, ZeroPeer};
use periph::edma::{self, TcdView};
use periph::regfile::RegFile;

#[test]
fn register_map_table_40_3() {
    // RM Table 40-3 p.1180: MCR at +0x00, SR at +0x2C, same layout on every
    // DSPI instance.
    assert_eq!(dspi::DSPI2_BASE, 0xEC03_8000);
    assert_eq!(dspi::DSPI2_MCR, 0xEC03_8000);
    assert_eq!(dspi::DSPI2_SR, 0xEC03_802C);
    assert_eq!(dspi::DSPI1_BASE, 0xFC03_C000);
    assert_eq!(dspi::DSPI1_MCR, 0xFC03_C000);
    assert_eq!(dspi::DSPI1_SR, 0xFC03_C02C);
}

#[test]
fn sr_eoqf_bit_28_table_40_7() {
    // RM Table 40-7 p.1189: EOQF (bit 28) "set when the TX FIFO entry has
    // the EOQ bit set... and after the last incoming databit is sampled" --
    // `FUN_400cd2bc`'s own "link idle" poll (`emu/dspiframe.py`'s naming).
    assert_eq!(dspi::SR_LINK_IDLE, 1 << 28);
}

#[test]
fn channel_roles_and_vectors() {
    // emu/dspiframe.py: TX_CHAN, RX_CHAN = 29, 28; vector = channel + 120.
    assert_eq!(dspi::TX_CHAN, 29);
    assert_eq!(dspi::RX_CHAN, 28);
    assert_eq!(dspi::TX_VECTOR, 149);
    assert_eq!(dspi::RX_VECTOR, 148);
}

/// `elem_bits`: the field `Dspi2Link::capture`/`run_deliver` actually reads
/// for this channel's element size -- `attr & 0x7` for a TX-side capture,
/// `(attr >> 8) & 0x7` for an RX-side deliver (`emu/dspi2.py`'s own
/// `_capture`/`_deliver`; ported verbatim in `dspi.rs`, even though RM Table
/// 19-21 p.385 names bits 2-0 `DSIZE` and 10-8 `SSIZE` -- the oracle's field
/// selection is what a replay must match, not the manual's naming for it).
/// Pass the raw 3-bit size code (`0b010` = 32-bit) already shifted to
/// whichever position the channel's role reads.
fn program(regs: &mut RegFile, chan: usize, attr: u16, nbytes: u32, citer: u16) {
    let base = edma::tcd_offset(chan);
    regs.set_u16_at(base + edma::ATTR, attr);
    regs.set_u32_at(base + edma::NBYTES, nbytes);
    regs.set_u16_at(base + edma::CITER, citer);
    regs.set_u16_at(base + edma::BITER, citer);
    regs.set_u32_at(base + edma::SLAST, 0);
    regs.set_u32_at(base + edma::DLAST, 0);
}

#[test]
fn capture_strips_pushr_tag_keeping_low_16_bits() {
    // `emu/dspi2.py::_capture`: a 4-byte source element is
    // `0x8001xxxx` (CONT|PCS0 tag, TXDATA) -- only the low 16 bits reach
    // the SHARC. `_capture` reads `attr & 0x7`; 0b010 (32-bit) -> ATTR=0x02.
    let mut regs = RegFile::new();
    program(&mut regs, TX_CHAN, 0x0002, 4, 2); // 2 minor loops of one 4-byte element each
    let mut link = Dspi2Link::new(TX_CHAN, RX_CHAN, true);
    let source = [0x80, 0x01, 0x00, 0x01, 0x80, 0x01, 0x00, 0x02];
    let frame = link.capture(&mut regs, &source);
    assert_eq!(frame, vec![0x00, 0x01, 0x00, 0x02]);
}

#[test]
fn capture_advances_saddr_and_reloads_citer() {
    let mut regs = RegFile::new();
    program(&mut regs, TX_CHAN, 0x0002, 4, 3); // 3 elements of 4 bytes
    let base = edma::tcd_offset(TX_CHAN);
    regs.set_u16_at(base + edma::SOFF, 4); // post-increment by the element size
    TcdView::new(&mut regs, TX_CHAN).set_saddr(0x8000_1BC0);
    let mut link = Dspi2Link::new(TX_CHAN, RX_CHAN, true);
    let source = [0u8; 12];
    link.capture(&mut regs, &source);
    let tcd = TcdView::new(&mut regs, TX_CHAN);
    assert_eq!(tcd.saddr(), 0x8000_1BC0 + 12); // 3 elements * SOFF(4), SLAST=0
    assert_eq!(tcd.citer(), 3); // CITER<-BITER on major completion
}

#[test]
fn zero_peer_returns_silence_of_the_right_length() {
    let mut peer = ZeroPeer;
    assert_eq!(peer.exchange(&[1, 2, 3]), vec![0, 0, 0]);
    assert_eq!(peer.exchange(&[]), Vec::<u8>::new());
}

#[test]
fn full_frame_round_trip_arms_tx_then_rx_and_delivers_zeros() {
    // The documented order (`emu/dspi2.py`'s module docstring): TX (29)
    // captured first, RX (28) armed second, which is what triggers the
    // exchange and the RX destination write.
    let mut regs = RegFile::new();
    program(&mut regs, TX_CHAN, 0x0000, 2, 4); // 8-bit elements, no tag stripping
    program(&mut regs, RX_CHAN, 0x0000, 2, 4);
    TcdView::new(&mut regs, RX_CHAN).set_daddr(0x8000_1000);
    let mut link = Dspi2Link::new(TX_CHAN, RX_CHAN, true);
    let mut peer = ZeroPeer;

    let tx_bytes = [1u8, 2, 3, 4, 5, 6, 7, 8];
    let captured = link.capture(&mut regs, &tx_bytes);
    assert_eq!(captured, tx_bytes); // 8-bit elements: no PUSHR tag to strip

    let write = link.arm_rx(&mut regs, &mut peer).expect("exchange ran");
    assert_eq!(write.addr, 0x8000_1000);
    assert_eq!(write.data, vec![0u8; 8]);
    assert_eq!(link.frames, 1);
    assert_eq!(link.tx_bytes, 8);
}

#[test]
fn touches_and_arms_match_saer_and_channel_number() {
    let link = Dspi2Link::new(TX_CHAN, RX_CHAN, true);
    assert!(link.touches(TX_CHAN as u8));
    assert!(link.touches(RX_CHAN as u8));
    assert!(!link.touches(35)); // UART8 TX, a different channel entirely
    assert!(link.touches(0x40)); // SAER: set-all
    assert!(!link.touches(0x80 | TX_CHAN as u8)); // NOP bit set
    assert!(link.arms_tx(TX_CHAN as u8) && !link.arms_tx(RX_CHAN as u8));
    assert!(link.arms_rx(RX_CHAN as u8) && !link.arms_rx(TX_CHAN as u8));
}
