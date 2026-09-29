//! Unit tests against MCF5441XRM chapter 35 (SSI) facts already recovered
//! and cited in `emu/ssi.py`'s own docstring, plus `ssi::Ssi0Dma`'s exact
//! (non-coalescing) port of it. No trace exercises this bank (see `ssi.rs`'s
//! module docs), so these are the only check it gets.

use periph::edma::{self, TcdView};
use periph::regfile::RegFile;
use periph::ssi::{self, RX_CHAN, RxHandoverPeer, Ssi0Dma, TX_CHAN};

#[test]
fn channel_roles_and_vectors() {
    // emu/ssi.py: RX_CHAN, TX_CHAN = 48, 50; RX_VECTOR/TX_VECTOR/FORCE_VECTOR
    // = 168/170/191.
    assert_eq!(RX_CHAN, 48);
    assert_eq!(TX_CHAN, 50);
    assert_eq!(ssi::RX_VECTOR, 168);
    assert_eq!(ssi::TX_VECTOR, 170);
    assert_eq!(ssi::FORCE_VECTOR, 191);
}

#[test]
fn audio_request_rate_derivation() {
    // emu/ssi.py's docstring: FCSR watermark 8 of 16 active slots -> 2
    // requests per SSI audio frame; 48 kHz frame rate (assumed, not
    // recovered) -> 2 * 48,000 = 96,000 Hz. TCD major loop 64 minors -> the
    // TX major interrupt at 96,000/64 = 1,500 Hz, matching the SHARC's own
    // known 1,500 blocks/s cadence -- see that module's docstring for the
    // three independent corroborations.
    assert_eq!(ssi::AUDIO_SSI0_REQUEST_HZ, 96_000);
    assert_eq!(ssi::AUDIO_SSI0_REQUEST_HZ / 64, 1_500);
}

#[test]
fn rx_handover_marker_at_major_loop_start_only() {
    // `RxHandoverPeer`: `0x007FFFFF` written only when this call is the
    // major loop's first request (`emu/ssi.py`'s `_at_major_start`).
    let mut peer = RxHandoverPeer;
    let first = peer.rx_major(32, true);
    assert_eq!(&first[0..4], &RxHandoverPeer::MARKER.to_be_bytes());
    let later = peer.rx_major(32, false);
    assert_eq!(&later[0..4], &[0, 0, 0, 0]);
    assert_eq!(first.len(), 32);
}

fn program(regs: &mut RegFile, chan: usize, saddr: u32, daddr: u32, citer: u16) {
    let base = edma::tcd_offset(chan);
    regs.set_u16_at(base + edma::ATTR, 0x0202); // 32-bit source and dest
    regs.set_u32_at(base + edma::NBYTES, 32);
    regs.set_u16_at(base + edma::CITER, citer);
    regs.set_u16_at(base + edma::BITER, citer);
    regs.set_u16_at(base + edma::SOFF, 4);
    regs.set_u16_at(base + edma::DOFF, 4);
    regs.set_u32_at(base + edma::SLAST, 0);
    regs.set_u32_at(base + edma::DLAST, 0);
    TcdView::new(regs, chan).set_saddr(saddr);
    TcdView::new(regs, chan).set_daddr(daddr);
}

#[test]
fn run_minor_advances_addresses_and_counts_major_completion() {
    let mut regs = RegFile::new();
    program(&mut regs, TX_CHAN, 0x8000_0000, 0, 2);
    let mut dma = Ssi0Dma::new(96_000, 132_000_000);
    dma.enabled[TX_CHAN] = true;

    let r1 = dma.run_minor(&mut regs, TX_CHAN, None).unwrap();
    assert!(!r1.major_complete);
    assert_eq!(TcdView::new(&mut regs, TX_CHAN).saddr(), 0x8000_0000 + 32);

    let r2 = dma.run_minor(&mut regs, TX_CHAN, None).unwrap();
    assert!(r2.major_complete);
    assert_eq!(dma.major_loops_tx, 1);
    assert_eq!(TcdView::new(&mut regs, TX_CHAN).citer(), 2); // reloaded from BITER
}

#[test]
fn run_minor_disabled_channel_is_a_no_op() {
    let mut regs = RegFile::new();
    program(&mut regs, RX_CHAN, 0, 0x8000_0000, 1);
    let mut dma = Ssi0Dma::new(96_000, 132_000_000);
    assert!(dma.run_minor(&mut regs, RX_CHAN, None).is_none());
}

#[test]
fn on_serq_enables_named_or_all_channels() {
    let mut dma = Ssi0Dma::new(96_000, 132_000_000);
    dma.on_serq(TX_CHAN as u8);
    assert!(dma.enabled[TX_CHAN] && !dma.enabled[RX_CHAN]);
    let mut dma2 = Ssi0Dma::new(96_000, 132_000_000);
    dma2.on_serq(0x40); // SAER: set-all
    assert!(dma2.enabled[TX_CHAN] && dma2.enabled[RX_CHAN]);
}

#[test]
fn intfrch1_bit31_tracks_the_forced_vector() {
    let mut dma = Ssi0Dma::new(96_000, 132_000_000);
    assert!(!dma.vector191_owed());
    dma.on_intfrch1(ssi::INTFRCH1_SOURCE63);
    assert!(dma.vector191_owed());
    dma.mark_vector191_delivered();
    assert!(!dma.vector191_owed());
    dma.on_intfrch1(0);
    assert!(!dma.force_asserted);
}
