//! Changing the instruction rate keeps every armed deadline at the same emulated time
//! (`DIGI_CF_IPS` restores a 132 MHz snapshot, then rescales).

use periph::dtim::DtimBank;
use periph::pit::{BASES, F_BUS, PitBank};
use periph::ssi::Ssi0Dma;

#[test]
fn pit_deadline_keeps_emulated_time() {
    // PRE=0, PMR=131: 1 us period = 132 instructions at ips = F_BUS.
    let mut bank = PitBank::new(vec![1], F_BUS, false);
    bank.write(BASES[1], 2, 0x0B);
    bank.write(BASES[1] + 2, 2, 131);
    assert_eq!(bank.deadline(1000), Some(1132));
    // At tick 1000 the remaining 132 instructions become 264 at double the rate.
    bank.rescale(1000.0, 2.0 * F_BUS);
    assert_eq!(bank.next_deadline(1), Some(1264.0));
    assert_eq!(
        bank.period(1),
        Some(264.0),
        "later periods use the new rate"
    );
    bank.rescale(1000.0, F_BUS / 4.0);
    assert_eq!(bank.next_deadline(1), Some(1033.0));
    assert_eq!(bank.period(1), Some(33.0));
}

#[test]
fn dtim_deadline_keeps_emulated_time() {
    let mut bank = DtimBank::new(vec![3], F_BUS, false);
    // DTMR: PS=0, CE=0b01 (rising), OM=0, ORRI|FRR|CLK=bus/1, RST. DTRR gives the reference.
    let base = periph::dtim::BASES[3];
    bank.write(base + 4, 4, 131); // DTRR
    bank.write(base, 2, 0x0013);
    let d = bank.deadline(0).expect("armed");
    bank.rescale(0.0, 2.0 * F_BUS);
    let after = bank.next_deadline(3).unwrap();
    assert!((after - 2.0 * d as f64).abs() <= 1.0, "{after} vs {d}");
}

#[test]
fn ssi_deadline_keeps_emulated_time() {
    let mut dma = Ssi0Dma::new(96_000, 132_000_000);
    dma.on_serq(0x40);
    let before = dma.step(0, None); // instructions to the first request
    assert!(before > 0);
    dma.rescale_at(0, 264_000_000);
    let after = dma.step(0, None);
    assert!(
        (after as i64 - 2 * before as i64).abs() <= 1,
        "{after} vs {before}"
    );
}
