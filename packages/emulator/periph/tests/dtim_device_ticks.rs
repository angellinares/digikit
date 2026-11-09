//! Completed post-prescaler tick abstraction, not a bus clock or subcycle model.
use periph::dtim::{BASES, DeviceReferenceError, DtimBank, VECTORS};

fn configured(reference: u32, counter: u32, mode: u16, extended: u8, events: u8) -> DtimBank {
    let mut bank = DtimBank::new_device(vec![], 1.0, true);
    let mut page = [0; 16];
    page[..2].copy_from_slice(&mode.to_be_bytes());
    page[2] = extended;
    page[3] = events;
    page[4..8].copy_from_slice(&reference.to_be_bytes());
    page[12..16].copy_from_slice(&counter.to_be_bytes());
    assert!(bank.load_page(BASES[0], &page));
    bank
}

fn state(bank: &DtimBank) -> Vec<u8> {
    let mut writer = periph::snap::Writer::new();
    bank.snap_save(&mut writer);
    writer.buf
}

#[test]
fn inclusive_reference_period_restarts_and_repeats_after_w1c() {
    let mut bank = configured(2, 0, 0x001d, 0, 1);
    let initial = state(&bank);
    bank.device_advance_ticks(0, 0).unwrap();
    assert_eq!(state(&bank), initial);
    for count in [1, 2] {
        bank.device_advance_ticks(0, 1).unwrap();
        assert_eq!(bank.read(BASES[0] + 12, 4), Some(count));
        assert_eq!(bank.read(BASES[0] + 3, 1), Some(1));
    }
    bank.device_advance_ticks(0, 1).unwrap();
    assert_eq!(bank.read(BASES[0] + 12, 4), Some(0));
    assert_eq!(bank.read(BASES[0] + 3, 1), Some(3));
    assert_eq!(bank.device_reference_vectors().unwrap(), [VECTORS[0]]);
    assert!(bank.device_write(BASES[0] + 3, 1, 2));
    bank.device_advance_ticks(0, 3).unwrap(); // no DTRR update/reset needed
    assert_eq!(bank.read(BASES[0] + 3, 1), Some(3));
    bank.device_advance_ticks(0, 11).unwrap(); // several periods coalesce
    assert_eq!(bank.read(BASES[0] + 12, 4), Some(2));
    assert_eq!(bank.read(BASES[0] + 3, 1), Some(3));
    assert!(!bank.pending(0)); // Oracle scheduling/bookkeeping is untouched
    assert_eq!(bank.fired(0), 0);
    assert_eq!(bank.next_deadline(0), None);
}

#[test]
fn tick_chunks_match_whole_advance_including_u64_max_and_u32_reference_max() {
    let chunks = [0, 1, 17, 1_u64 << 32, u64::MAX - (1_u64 << 32) - 18];
    for reference in [0, 1, 2, 0x43238, u32::MAX] {
        for counter in [0, reference / 2, reference] {
            let mut whole = configured(reference, counter, 0x001b, 0, 0x81);
            let mut chunked = configured(reference, counter, 0x001b, 0, 0x81);
            whole.device_advance_ticks(0, u64::MAX).unwrap();
            for ticks in chunks {
                chunked.device_advance_ticks(0, ticks).unwrap();
            }
            assert_eq!(
                state(&whole),
                state(&chunked),
                "reference={reference} counter={counter}"
            );
            assert_eq!(whole.read(BASES[0] + 3, 1), Some(0x83));
        }
    }
    let mut zero = configured(0, 0, 0x001b, 0, 0);
    zero.device_advance_ticks(0, 0).unwrap();
    assert_eq!(zero.read(BASES[0] + 3, 1), Some(0));
    zero.device_advance_ticks(0, 1).unwrap();
    assert_eq!(zero.read(BASES[0] + 12, 4), Some(0));
    assert_eq!(zero.read(BASES[0] + 3, 1), Some(2));
    let mut max = configured(u32::MAX, 0, 0x001b, 0, 0);
    max.device_advance_ticks(0, 1_u64 << 32).unwrap();
    assert_eq!(max.read(BASES[0] + 12, 4), Some(0));
    assert_eq!(max.read(BASES[0] + 3, 1), Some(2));
    let mut max = configured(u32::MAX, u32::MAX, 0x001b, 0, 0);
    max.device_advance_ticks(0, u64::MAX).unwrap();
    assert_eq!(max.read(BASES[0] + 12, 4), Some(u32::MAX - 1));
}

#[test]
fn manual_polling_example_counts_five_timeouts_without_reference_rewrites() {
    // §39.4.1 p1171/p1172: PS=255, FRR=1, CLK=bus/16, ORRI=0.
    // The caller has already divided the clock; one period is 44,976 ticks.
    let mut bank = configured(0xafaf, 0, 0xff0d, 0, 0);
    for _ in 0..5 {
        bank.device_advance_ticks(0, 44_975).unwrap();
        assert_eq!(bank.read(BASES[0] + 12, 4), Some(0xafaf));
        assert_eq!(bank.read(BASES[0] + 3, 1), Some(0));
        bank.device_advance_ticks(0, 1).unwrap();
        assert_eq!(bank.read(BASES[0] + 12, 4), Some(0));
        assert_eq!(bank.read(BASES[0] + 3, 1), Some(2));
        assert!(bank.device_reference_vectors().unwrap().is_empty());
        assert!(bank.device_write(BASES[0] + 3, 1, 2));
    }
    bank.device_advance_ticks(0, 44_976).unwrap();
    assert!(bank.device_write(BASES[0], 2, 0xff1d)); // late ORRI enable
    assert_eq!(bank.device_reference_vectors().unwrap(), [VECTORS[0]]);
}

#[test]
fn inactive_timers_preserve_counter_and_events_but_advanced_modes_still_reject() {
    for mode in [0x001c, 0x0019, 0] {
        // Inactive state is preserved even if outside the active restart contract.
        let mut bank = configured(2, 3, mode, 0, 0x83);
        let before = state(&bank);
        bank.device_advance_ticks(0, u64::MAX).unwrap();
        assert_eq!(state(&bank), before);
        for extended in [0x80, 0x20, 0x40, 0x10, 1] {
            let mut bank = configured(2, 3, mode, extended, 0x83);
            let before = state(&bank);
            assert_eq!(
                bank.device_advance_ticks(0, 0),
                Err(DeviceReferenceError::UnsupportedMode { channel: 0 })
            );
            assert_eq!(state(&bank), before);
        }
    }
}

#[test]
fn unsupported_active_modes_and_out_of_range_counters_reject_atomically() {
    for (mode, extended) in [
        (0x0015, 0),
        (0x001f, 0),
        (0x005d, 0),
        (0x001d, 0x80),
        (0x001d, 0x20),
    ] {
        let mut bank = configured(2, 1, mode, extended, 1);
        let before = state(&bank);
        for ticks in [0, u64::MAX] {
            assert_eq!(
                bank.device_advance_ticks(0, ticks),
                Err(DeviceReferenceError::UnsupportedMode { channel: 0 })
            );
            assert_eq!(state(&bank), before);
        }
    }
    let mut bank = configured(2, 3, 0x001d, 0, 1);
    let before = state(&bank);
    for ticks in [0, u64::MAX] {
        assert_eq!(
            bank.device_advance_ticks(0, ticks),
            Err(DeviceReferenceError::CounterOutOfRange {
                channel: 0,
                counter: 3,
                reference: 2
            })
        );
        assert_eq!(state(&bank), before);
    }
    assert_eq!(
        bank.device_advance_ticks(4, 1),
        Err(DeviceReferenceError::InvalidChannel { channel: 4 })
    );
    assert_eq!(state(&bank), before);
}

#[test]
fn existing_register_snapshot_retains_the_completed_tick_phase() {
    let mut original = configured(2, 0, 0x001d, 0, 1);
    original.device_advance_ticks(0, 2).unwrap();
    let saved = state(&original);
    let mut restored = DtimBank::new_device(vec![], 9_999.0, false);
    restored
        .snap_load(&mut periph::snap::Reader::new(&saved))
        .unwrap();
    assert_eq!(restored.read(BASES[0] + 12, 4), Some(2));
    assert_eq!(restored.read(BASES[0] + 3, 1), Some(1));
    restored.device_advance_ticks(0, 1).unwrap();
    assert_eq!(restored.read(BASES[0] + 12, 4), Some(0));
    assert_eq!(restored.read(BASES[0] + 3, 1), Some(3));
}
