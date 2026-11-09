//! Explicit supplied events and bounded register acknowledgement, not timer timing.
use periph::pit::{BASES, PitBank, VECTORS};

#[test]
fn timeout_status_and_requests_are_independent_of_oracle_topology_and_enable() {
    let mut pit = PitBank::new(vec![], 1.0, true);
    for channel in 0..4 {
        assert!(pit.device_latch_timeout(channel));
        assert_eq!(pit.read(BASES[channel], 2), Some(4));
        assert!(pit.device_pending_vectors().is_empty());
        // PIE exposes the previously latched event even with EN clear.
        assert!(pit.device_write(BASES[channel], 2, 8));
        assert_eq!(pit.device_pending_vectors(), vec![VECTORS[channel]]);
        assert!(pit.device_write(BASES[channel], 2, 9));
        assert_eq!(pit.read(BASES[channel], 2), Some(13));
        assert!(pit.device_write(BASES[channel], 2, 8)); // stop EN
        assert_eq!(pit.device_pending_vectors(), vec![VECTORS[channel]]);
        assert!(pit.device_write(BASES[channel], 2, 0)); // mask PIE
        assert_eq!(pit.read(BASES[channel], 2), Some(4));
        assert!(pit.device_pending_vectors().is_empty());
        assert!(!pit.pending(channel));
        assert_eq!(pit.fired(channel), 0);
        assert_eq!(pit.next_deadline(channel), None);
    }
    for base in BASES {
        assert!(pit.device_write(base, 2, 8));
    }
    assert_eq!(pit.device_pending_vectors(), VECTORS);
    assert_eq!(pit.device_pending_vectors(), VECTORS); // read is not consumption
}

#[test]
fn pcsr_w1c_and_pmr_acknowledge_without_oracle_bookkeeping() {
    let mut pit = PitBank::default();
    for ovw in [0, 0x10] {
        assert!(pit.device_write(BASES[0], 2, ovw | 0x0b));
        assert!(pit.device_latch_timeout(0));
        assert!(pit.device_write(BASES[0], 2, ovw | 0x0b)); // zero preserves PIF
        assert_eq!(pit.read(BASES[0], 2), Some(ovw | 0x0f));
        assert!(pit.device_write(BASES[0], 2, ovw | 0x0f));
        assert_eq!(pit.read(BASES[0], 2), Some(ovw | 0x0b));
        assert!(pit.device_pending_vectors().is_empty());
        assert!(pit.device_write(BASES[0], 2, ovw | 0x0f)); // cannot set PIF
        assert_eq!(pit.read(BASES[0], 2), Some(ovw | 0x0b));
        assert!(pit.device_latch_timeout(0));
        assert!(pit.device_write(BASES[0] + 2, 2, 0x1234));
        assert_eq!(pit.read(BASES[0] + 2, 2), Some(0x1234));
        assert_eq!(pit.read(BASES[0], 2), Some(ovw | 0x0b));
        assert!(pit.device_pending_vectors().is_empty());
    }
    assert_eq!(pit.cleared(0), 0);
    assert_eq!(pit.fired(0), 0);
}

#[test]
fn unsupported_accesses_and_channels_reject_without_register_mutation() {
    let mut pit = PitBank::default();
    assert!(pit.device_write(BASES[0], 2, 0x0b));
    assert!(pit.device_write(BASES[0] + 2, 2, 0x1234));
    assert!(pit.device_latch_timeout(0));
    for (addr, size) in [
        (BASES[0], 1),
        (BASES[0] + 1, 1),
        (BASES[0], 4),
        (BASES[0] + 1, 2),
        (BASES[0] + 2, 4),
        (BASES[0] + 4, 2),
        (BASES[0] + 0x3fff, 2),
        (0, 2),
    ] {
        assert!(!pit.device_write(addr, size, u32::MAX));
        assert_eq!(pit.read(BASES[0], 4), Some(0x000f1234));
        assert_eq!(pit.read(BASES[0] + 4, 2), Some(0));
    }
    assert!(!pit.device_latch_timeout(4));
    assert!(!pit.device_latch_timeout(usize::MAX));
    assert_eq!(pit.read(BASES[0], 4), Some(0x000f1234));
    assert_eq!(pit.device_pending_vectors(), vec![VECTORS[0]]);
}

#[test]
fn oracle_pcsr_and_pmr_remain_plain_ram() {
    let mut pit = PitBank::default();
    assert!(pit.write(BASES[0], 2, 0x000f));
    assert_eq!(pit.read(BASES[0], 2), Some(0x000f));
    assert!(pit.write(BASES[0] + 2, 2, 0x1234));
    assert_eq!(pit.read(BASES[0], 2), Some(0x000f));
    assert!(pit.write(BASES[0], 1, 0xab));
    assert_eq!(pit.read(BASES[0], 2), Some(0xab0f));
}
