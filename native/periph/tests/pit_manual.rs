//! Unit tests against MCF5441XRM chapter 38 (Programmable Interrupt
//! Timers), independent of any trace: register math and semantics as the
//! manual specifies them.

use periph::intc::IntcBank;
use periph::pit::{BASES, F_BUS, PitBank, VECTORS};
use periph::sr::SrTracker;

fn write_pcsr_pmr(bank: &mut PitBank, ch: usize, pcsr: u16, pmr: u16) {
    let base = BASES[ch];
    bank.write(base, 2, pcsr as u32);
    bank.write(base + 2, 2, pmr as u32);
}

/// Locate `vector`'s (controller base, source number).
fn locate_vector(vector: u16) -> (u32, u32) {
    for i in 0..3 {
        let first = periph::intc::VECTOR_BASE[i];
        if vector >= first && vector < first + 64 {
            return (periph::intc::BASES[i], (vector - first) as u32);
        }
    }
    panic!("vector {vector} out of range");
}

/// An IntcBank with `vector`'s ICR level set and unmasked, as firmware would
/// leave it before enabling the timer (RM SS17.2.9/.2).
fn intc_with_level(vector: u16, level: u8) -> IntcBank {
    let mut intc = IntcBank::new();
    let (base, src) = locate_vector(vector);
    intc.write(base + 0x40 + src, 1, level as u32); // ICR
    // Clear IMR's mask bit for this source (reset is all-1s / masked).
    let imr_addr = if src < 32 { base + 0x0C } else { base + 0x08 };
    let cur = intc.read(imr_addr, 4).unwrap();
    let bit = 1u32 << (src % 32);
    intc.write(imr_addr, 4, cur & !bit);
    intc
}

#[test]
fn prescaler_table_38_3() {
    // RM Table 38-3: PRE selects a divisor of 2^PRE, 0000 -> 1 up to
    // 1111 -> 32768. EN|PIE set, PMR=0 so period = prescale / F_BUS * ips.
    let mut bank = PitBank::new(vec![0], 1.0, false);
    for pre in 0u16..=15 {
        let pcsr = 0x09 | (pre << 8); // EN|PIE, no RLD needed for period()
        write_pcsr_pmr(&mut bank, 0, pcsr, 0);
        let want = (1u64 << pre) as f64 / F_BUS;
        assert_eq!(bank.period(0), Some(want), "PRE={pre}");
    }
}

#[test]
fn disabled_channel_has_no_period() {
    let mut bank = PitBank::new(vec![0], 1.0, false);
    // PIE clear.
    write_pcsr_pmr(&mut bank, 0, 0x01, 100);
    assert_eq!(bank.period(0), None, "EN without PIE");
    // EN clear.
    write_pcsr_pmr(&mut bank, 0, 0x08, 100);
    assert_eq!(bank.period(0), None, "PIE without EN");
}

#[test]
fn one_microsecond_calibration() {
    // emu/pit.py's module docstring: the firmware's own delay routine
    // programs PRE=0, PMR=131 at the 132 MHz bus clock and gets exactly
    // 1 us per tick under `prescale*(pmr+1)/F_BUS`. ips=F_BUS here so the
    // result is directly in seconds.
    let mut bank = PitBank::new(vec![1], F_BUS, false);
    write_pcsr_pmr(&mut bank, 1, 0x0B, 131); // EN|PIE|RLD, PRE=0
    assert_eq!(bank.period(1), Some(132.0));
}

#[test]
fn pif_write_one_clear_discards_pending_before_delivery() {
    // RM Table 38-3: "Clear PIF by writing a 1 to it". `Pits._on_pcsr`
    // discards a pending tick the instant the guest does this, before the
    // CPU ever takes it (see pit.rs's write() docs).
    let intc = intc_with_level(VECTORS[0], 1);
    let mut sr = SrTracker::new();
    let mut bank = PitBank::new(vec![0], F_BUS, false);
    write_pcsr_pmr(&mut bank, 0, 0x0B, 131); // 1 us period
    bank.deadline(0); // arm
    let due = bank.next_deadline(0).unwrap().ceil() as u64;
    // Make it pending without delivering: hold IPL at the channel's own
    // level so `service` marks pending but cannot deliver.
    sr.on_taken(Some(1), 0); // raises tracked IPL to 1, blocking a level-1 source
    let raised = bank.service(due, &intc, &mut sr);
    assert!(raised.is_empty(), "blocked by IPL, should stay pending");
    assert!(bank.pending(0));
    // Guest write-1-to-clear on PCSR (byte containing PIF) discards it.
    bank.write(BASES[0] + 1, 1, 0x04);
    assert!(
        !bank.pending(0),
        "PIF write-1 should clear the pending tick"
    );
    assert_eq!(bank.cleared(0), 1);
}

#[test]
fn missed_tick_does_not_chase_the_backlog() {
    // pit.py's `Pits.service`: several elapsed periods produce exactly one
    // delivery, and the new deadline is `done + period` (drop the backlog),
    // not `old_deadline + period` (which would still be behind `done`).
    let intc = intc_with_level(VECTORS[0], 3);
    let mut sr = SrTracker::new();
    let mut bank = PitBank::new(vec![0], F_BUS, false);
    write_pcsr_pmr(&mut bank, 0, 0x0B, 131); // 1 us / 132 instr period
    bank.deadline(0);
    let period = bank.period(0).unwrap();
    let far_done = (period * 1000.0).ceil() as u64; // ~1000 periods later
    let raised = bank.service(far_done, &intc, &mut sr);
    assert_eq!(raised, vec![(VECTORS[0], 3)]);
    let next = bank.next_deadline(0).unwrap();
    assert!(
        next > far_done as f64 && next <= far_done as f64 + period + 1.0,
        "next={next} should resync off done={far_done}, not chase the backlog"
    );
}

#[test]
fn priority_within_a_level_is_highest_source_first() {
    // RM SS17.3.1/Table 17-19: within one level, the higher source number
    // is serviced first. PIT0/2/3 are INTC2 sources 13/15/16, so `(3, 2, 0)`
    // (emu.pit.Pits' own default order) offers PIT3 before PIT0 when both
    // are pending at the same level. Taking PIT3's interrupt raises IPL to
    // that level, which then refuses PIT0's same-level request until PIT3's
    // handler returns (the class docs' "the first one taken raises IPL to
    // its own level, which refuses any later one at the same level") -- so
    // only PIT3 delivers this call, and PIT0 is left pending, still in
    // priority order.
    let mut intc = IntcBank::new();
    for (vec, lvl) in [(VECTORS[0], 3u8), (VECTORS[3], 3u8)] {
        let (base, src) = locate_vector(vec);
        intc.write(base + 0x40 + src, 1, lvl as u32);
        let imr_addr = base + 0x0C; // both sources < 32
        let cur = intc.read(imr_addr, 4).unwrap();
        intc.write(imr_addr, 4, cur & !(1u32 << src));
    }
    let mut sr = SrTracker::new();
    let mut bank = PitBank::new(vec![3, 2, 0], F_BUS, false);
    write_pcsr_pmr(&mut bank, 0, 0x0B, 131);
    write_pcsr_pmr(&mut bank, 3, 0x0B, 131);
    bank.deadline(0);
    let due = bank
        .next_deadline(0)
        .unwrap()
        .max(bank.next_deadline(3).unwrap());
    let raised = bank.service(due.ceil() as u64, &intc, &mut sr);
    assert_eq!(
        raised,
        vec![(VECTORS[3], 3)],
        "PIT3 (source 16) must be offered first"
    );
    assert!(
        bank.pending(0),
        "PIT0 stays pending, masked by PIT3's own IPL"
    );
}

#[test]
fn masked_or_disabled_source_holds_the_tick_pending() {
    let intc = IntcBank::new(); // ICR level 0 everywhere: every source disabled
    let mut sr = SrTracker::new();
    let mut bank = PitBank::new(vec![0], F_BUS, false);
    write_pcsr_pmr(&mut bank, 0, 0x0B, 131);
    bank.deadline(0);
    let due = bank.next_deadline(0).unwrap().ceil() as u64;
    let raised = bank.service(due, &intc, &mut sr);
    assert!(raised.is_empty());
    assert!(
        bank.pending(0),
        "a disabled (level-0) source should stay pending"
    );
}

#[test]
fn off_channel_is_plain_ram() {
    // A channel outside `channels` gets no scheduling logic at all: writes
    // and reads just round-trip (module docs, "unmodelled registers").
    let mut bank = PitBank::new(vec![0], F_BUS, false); // channel 1 not listed
    bank.write(BASES[1], 2, 0x1234);
    bank.write(BASES[1] + 2, 2, 0x5678);
    assert_eq!(bank.read(BASES[1], 2), Some(0x1234));
    assert_eq!(bank.read(BASES[1] + 2, 2), Some(0x5678));
    // period() is available even for a non-serviced channel, but service()
    // never looks at it since it is not in `channels`.
    assert_eq!(bank.channels(), &[0]);
}
