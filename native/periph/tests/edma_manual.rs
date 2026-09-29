//! Unit tests against MCF5441XRM chapter 19 (eDMA), independent of any
//! trace: register semantics as the manual specifies them, plus
//! `TxChannel`'s port of `emu/edma.py`'s `TxChannel.run`.

use periph::edma::{self, TcdView, TxChannel};
use periph::regfile::RegFile;

#[test]
fn serq_cerq_ceei_cint_read_as_zero() {
    // RM p.374 EDMA_SERQ / p.375 EDMA_CERQ: "Reads of this register return
    // all zeroes"; same wording for EDMA_CEEI (p.376) and EDMA_CINT (p.377).
    // `edma::READ_ZERO` is what `spilink::DmaLink::read` checks before
    // falling through to plain `RegFile` passthrough -- see its own docs.
    assert_eq!(edma::SERQ, 0xFC04_4018);
    assert_eq!(edma::CERQ, 0xFC04_4019);
    assert_eq!(edma::SEEI, 0xFC04_401A);
    assert_eq!(edma::CEEI, 0xFC04_401B);
    assert_eq!(edma::CINT, 0xFC04_401C);
    assert_eq!(
        edma::READ_ZERO,
        [edma::SERQ, edma::CERQ, edma::CEEI, edma::CINT]
    );
}

#[test]
fn tcd_csr_bit_positions_table_19_31() {
    // RM Figure 19-36 / Table 19-31 p.390-391.
    assert_eq!(edma::CSR_DONE, 0x0080);
    assert_eq!(edma::CSR_ACTIVE, 0x0040);
    assert_eq!(edma::CSR_MAJOR_E_LINK, 0x0020);
    assert_eq!(edma::CSR_E_SG, 0x0010);
    assert_eq!(edma::CSR_D_REQ, 0x0008);
    assert_eq!(edma::CSR_INT_HALF, 0x0004);
    assert_eq!(edma::CSR_INT_MAJOR, 0x0002);
    assert_eq!(edma::CSR_START, 0x0001);
}

#[test]
fn tcd_field_offsets_figure_19_24() {
    // RM Figure 19-24 p.384: the ColdFire word order (CITER at +0x14,
    // BITER at +0x1C -- NOT the Kinetis order, `emu/edma.py`'s own comment).
    assert_eq!(edma::SADDR, 0x00);
    assert_eq!(edma::ATTR, 0x04);
    assert_eq!(edma::SOFF, 0x06);
    assert_eq!(edma::NBYTES, 0x08);
    assert_eq!(edma::SLAST, 0x0C);
    assert_eq!(edma::DADDR, 0x10);
    assert_eq!(edma::CITER, 0x14);
    assert_eq!(edma::DOFF, 0x16);
    assert_eq!(edma::DLAST, 0x18);
    assert_eq!(edma::BITER, 0x1C);
    assert_eq!(edma::CSR, 0x1E);
    assert_eq!(edma::tcd_offset(1) - edma::tcd_offset(0), 0x20);
}

#[test]
fn signed_sign_extends() {
    assert_eq!(edma::signed(0xFFFF, 16), -1);
    assert_eq!(edma::signed(0x0001, 16), 1);
    assert_eq!(edma::signed(0xFFFF_FFFF, 32), -1);
    assert_eq!(edma::signed(1, 32), 1);
}

/// Programs TCD35 exactly as `docs/plan-native-emulator.md`'s cited layout
/// for the console UART8 TX ring (`emu/edma.py`'s module docstring):
/// `SADDR=ring`, `ATTR=0x6000` (SMOD 12), `NBYTES=1`, `DADDR=UDR8`, `DOFF=0`.
fn program_uart8_tx(regs: &mut RegFile, chan: usize, ring: u32, citer: u16) {
    let mut tcd = TcdView::new(regs, chan);
    tcd.set_saddr(ring);
    tcd.set_citer(citer);
    let base = edma::tcd_offset(chan);
    regs.set_u16_at(base + edma::ATTR, 0x6000);
    regs.set_u16_at(base + edma::SOFF, 1); // post-increment one byte per minor loop
    regs.set_u32_at(base + edma::NBYTES, 1);
    regs.set_u32_at(base + edma::SLAST, 0);
    regs.set_u16_at(base + edma::BITER, citer);
}

#[test]
fn tx_channel_run_advances_saddr_with_smod_wraparound() {
    // A 4096-byte ring at 0x4FE1B000 (`emu/edma.py`'s `TX_STATE`/`WAIT_LOOP`
    // docstring), SMOD 12 -> mask 0xFFF: SADDR wraps within the ring instead
    // of running off the end, exactly like `Pits`' channel bookkeeping wraps
    // -- see `emu/edma.py::TxChannel.run`.
    let mut regs = RegFile::new();
    let ring = 0x4FE1B000u32;
    program_uart8_tx(&mut regs, 35, ring + 0xFFE, 4); // 2 bytes from wraparound
    let mut ch = TxChannel::new(35, 155);
    let moved = ch.run(&mut regs, &[0u8; 4]);
    assert!(moved);
    let after = TcdView::new(&mut regs, 35).saddr();
    // ring+0xFFE, +1 four times = ring+0x1002, masked to ring | (0x1002 &
    // 0xFFF) = ring + 2.
    assert_eq!(after, ring + 2);
    assert_eq!(ch.bytes, 4);
    assert_eq!(ch.transfers, 1);
    assert_eq!(ch.pending, 1);
}

#[test]
fn tx_channel_run_no_op_when_citer_zero() {
    let mut regs = RegFile::new();
    program_uart8_tx(&mut regs, 35, 0x4FE1B000, 0);
    let mut ch = TxChannel::new(35, 155);
    assert!(!ch.run(&mut regs, &[]));
    assert_eq!(ch.transfers, 0);
    assert_eq!(ch.pending, 0);
}

#[test]
fn tx_channel_run_reloads_citer_from_biter_on_major_completion() {
    // RM p.390: "As the major iteration count is exhausted, the contents of
    // this field [BITER] are reloaded into the CITER field."
    let mut regs = RegFile::new();
    program_uart8_tx(&mut regs, 35, 0x4FE1B000, 7);
    let mut ch = TxChannel::new(35, 155);
    ch.run(&mut regs, &[0u8; 7]);
    assert_eq!(TcdView::new(&mut regs, 35).citer(), 7);
}

#[test]
fn tx_channel_pending_consumed_in_order() {
    let mut ch = TxChannel::new(35, 155);
    assert!(!ch.consume_pending());
    let mut regs = RegFile::new();
    program_uart8_tx(&mut regs, 35, 0, 1);
    ch.run(&mut regs, &[0]);
    ch.run(&mut regs, &[0]);
    assert!(ch.consume_pending());
    assert!(ch.consume_pending());
    assert!(!ch.consume_pending());
}

#[test]
fn regfile_passthrough_for_unmodelled_tcd() {
    // TCD14/15 (DSPI1's would-be pair) and DSPI1/DSPI3's own registers stay
    // plain RAM, matching the Python oracle, which never intercepts them
    // either (`scratchpad/p4-mmio-recorder.md`'s "unmodelled registers").
    let mut regs = RegFile::new();
    let base = edma::tcd_offset(14);
    regs.write(base as u32, 4, 0xDEAD_BEEF);
    assert_eq!(regs.read(base as u32, 4), 0xDEAD_BEEF);
}
