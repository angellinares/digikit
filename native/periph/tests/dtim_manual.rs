//! Unit tests against MCF5441XRM chapter 39 (DMA Timers) and `emu/dtim.py`'s
//! documented rates, independent of any trace.

use periph::dtim::{BASES, DtimBank, F_BUS, VECTORS};
use periph::intc::IntcBank;
use periph::sr::SrTracker;

fn locate_vector(vector: u16) -> (u32, u32) {
    for i in 0..3 {
        let first = periph::intc::VECTOR_BASE[i];
        if vector >= first && vector < first + 64 {
            return (periph::intc::BASES[i], (vector - first) as u32);
        }
    }
    panic!("vector {vector} out of range");
}

fn intc_with_level(vector: u16, level: u8) -> IntcBank {
    let mut intc = IntcBank::new();
    let (base, src) = locate_vector(vector);
    intc.write(base + 0x40 + src, 1, level as u32);
    let imr_addr = if src < 32 { base + 0x0C } else { base + 0x08 };
    let cur = intc.read(imr_addr, 4).unwrap();
    intc.write(imr_addr, 4, cur & !(1u32 << (src % 32)));
    intc
}

fn write_dtmr_dtrr(bank: &mut DtimBank, ch: usize, dtmr: u16, dtxmr: u8, dtrr: u32) {
    let base = BASES[ch];
    bank.write(base, 2, dtmr as u32);
    bank.write(base + 2, 1, dtxmr as u32);
    bank.write(base + 4, 4, dtrr);
}

#[test]
fn dtim3_main_loop_tick_is_30_hz() {
    // emu/dtim.py's module docstring: DTMR3 = 0x001d (PS=0, CLK=10=bus/16,
    // ORRI, FRR, RST), DTRR3 = 0x43238, at 132 MHz -> ~30 Hz (the docstring
    // says 30.05; this checks the formula lands in the same neighbourhood,
    // not the last digit of a hand-transcribed hex constant).
    let mut bank = DtimBank::new(vec![3], F_BUS, false);
    write_dtmr_dtrr(&mut bank, 3, 0x001d, 0x00, 0x43238);
    let period = bank.period(3).unwrap();
    let hz = F_BUS / period;
    assert!((hz - 30.0).abs() < 0.1, "hz={hz}");
}

#[test]
fn dtim1_one_microsecond_sleep_calibration() {
    // module docstring: DTMR1 = 0x841b (PS=132 -> divide by 133, CLK=01 ->
    // bus/1, ORRI, FRR, RST). At 132 MHz the prescaler gives 992.5 kHz;
    // DTRR chosen for n microseconds is dtrr=n directly per
    // `0x40128c7c(n)`. One tick (DTRR=0) should be ~1.0075 us.
    let mut bank = DtimBank::new(vec![1], F_BUS, false);
    write_dtmr_dtrr(&mut bank, 1, 0x841b, 0x00, 0);
    let period_instr = bank.period(1).unwrap();
    let us = period_instr / F_BUS * 1e6;
    assert!((us - 1.0075).abs() < 0.001, "us={us}");
}

#[test]
fn stopped_or_external_clock_has_no_period() {
    let mut bank = DtimBank::new(vec![0], F_BUS, false);
    // CLK=00 (stop).
    write_dtmr_dtrr(&mut bank, 0, 0x0011, 0x00, 100); // RST|ORRI, CLK=00
    assert_eq!(bank.period(0), None);
    // CLK=11 (external pin) -- DTIM0/2 in the firmware, per the module docs.
    write_dtmr_dtrr(&mut bank, 0, 0x0017, 0x00, 100); // RST|ORRI, CLK=11
    assert_eq!(bank.period(0), None);
}

#[test]
fn dmaen_disables_the_timer() {
    // MCF5441XRM DTXMRn bit 7 (DMAEN): when set, ORRI interrupts don't fire.
    let mut bank = DtimBank::new(vec![0], F_BUS, false);
    write_dtmr_dtrr(&mut bank, 0, 0x001d, 0x80, 1000);
    assert_eq!(bank.period(0), None);
}

#[test]
fn due_tick_sets_ref_bit_in_guest_memory() {
    // Unlike PIT's PIF (never asserted by the model, see pit.rs), DTIM's
    // service DOES write DTER's REF bit (`Dtims.service`'s read-modify-write,
    // recorded as an HWR from `emu.dtim.Dtims.service`).
    let intc = intc_with_level(VECTORS[3], 2);
    let mut sr = SrTracker::new();
    let mut bank = DtimBank::new(vec![3], F_BUS, false);
    write_dtmr_dtrr(&mut bank, 3, 0x001d, 0x00, 100);
    bank.deadline(0);
    let due = bank.next_deadline(3).unwrap().ceil() as u64;
    let (raised, writes) = bank.service(due, &intc, &mut sr);
    assert_eq!(raised, vec![(VECTORS[3], 2)]);
    assert_eq!(writes.len(), 1);
    let (addr, byte) = writes[0];
    assert_eq!(addr, BASES[3] + DtimBank::DTER_OFFSET as u32);
    assert_ne!(byte & DtimBank::REF_BIT, 0);
}

#[test]
fn ref_write_one_clear_discards_pending_before_delivery() {
    let intc = intc_with_level(VECTORS[3], 2);
    let mut sr = SrTracker::new();
    let mut bank = DtimBank::new(vec![3], F_BUS, false);
    write_dtmr_dtrr(&mut bank, 3, 0x001d, 0x00, 100);
    bank.deadline(0);
    let due = bank.next_deadline(3).unwrap().ceil() as u64;
    sr.on_taken(Some(2), 0); // block delivery: IPL raised to the source's own level
    let (raised, _) = bank.service(due, &intc, &mut sr);
    assert!(raised.is_empty());
    assert!(bank.pending(3));
    bank.write(BASES[3] + 3, 1, 0x02); // DTER offset 3, REF bit
    assert!(!bank.pending(3));
    assert_eq!(bank.cleared(3), 1);
}

#[test]
fn highest_source_first_dtim3_before_dtim1() {
    // RM SS17.3.1 (Table 17-19): highest source number first within a
    // level. DTIM3 is INTC0 source 35, DTIM1 is source 33. Taking DTIM3's
    // interrupt raises IPL to its own level (2), which then refuses DTIM1's
    // same-level request until DTIM3's handler returns (`emu.pit.Pits`'
    // class docs, "the first one taken raises IPL to its own level, which
    // refuses any later one at the same level") -- so only DTIM3 delivers
    // this call; DTIM1 stays pending for the next boundary.
    let mut intc = IntcBank::new();
    for (vec, lvl) in [(VECTORS[1], 2u8), (VECTORS[3], 2u8)] {
        let (base, src) = locate_vector(vec);
        intc.write(base + 0x40 + src, 1, lvl as u32);
        // DTIM1/3 are INTC0 sources 33/35 (>= 32): IMRH, not IMRL.
        let imr_addr = if src < 32 { base + 0x0C } else { base + 0x08 };
        let cur = intc.read(imr_addr, 4).unwrap();
        intc.write(imr_addr, 4, cur & !(1u32 << (src % 32)));
    }
    let mut sr = SrTracker::new();
    let mut bank = DtimBank::new(vec![3, 1], F_BUS, false);
    // DTIM1 and DTIM3 have very different natural periods; force an exact
    // collision by loading both channels' deadlines at the same instant
    // directly, as a `STATE` checkpoint resync would.
    write_dtmr_dtrr(&mut bank, 1, 0x841b, 0x00, 100);
    write_dtmr_dtrr(&mut bank, 3, 0x001d, 0x00, 100);
    let next = [None, Some(1000.0), None, Some(1000.0)];
    let pending = [false; 4];
    bank.load_checkpoint(&next, &pending, false);
    let (raised, _) = bank.service(1000, &intc, &mut sr);
    assert_eq!(raised, vec![(VECTORS[3], 2)]);
    assert!(
        bank.pending(1),
        "DTIM1 stays pending, masked by DTIM3's own IPL"
    );
}

#[test]
fn off_channel_is_plain_ram() {
    let mut bank = DtimBank::new(vec![3], F_BUS, false); // channel 1 not listed
    bank.write(BASES[1], 2, 0x841b);
    assert_eq!(bank.read(BASES[1], 2), Some(0x841b));
}
