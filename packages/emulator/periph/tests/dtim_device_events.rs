//! Supplied reference events only: no counter timing, capture or DMA execution.
use periph::dtim::{BASES, DeviceReferenceError, DtimBank, VECTORS};

fn bank() -> DtimBank {
    DtimBank::new_device(vec![], 1.0, true)
}

#[test]
fn reference_status_survives_request_gating_on_all_physical_channels() {
    let mut bank = bank();
    for (channel, base) in BASES.into_iter().enumerate() {
        bank.device_latch_reference(channel).unwrap();
        assert_eq!(bank.read(base + 3, 1), Some(2));
        assert!(bank.device_reference_vectors().unwrap().is_empty());
        assert!(bank.device_write(base, 2, 0x001b));
        assert_eq!(bank.device_reference_vectors().unwrap(), [VECTORS[channel]]);
        assert!(bank.device_write(base + 2, 1, 0x80));
        assert!(bank.device_reference_vectors().unwrap().is_empty());
        assert_eq!(bank.read(base + 3, 1), Some(2));
        assert!(bank.device_write(base + 2, 1, 0));
        assert!(bank.device_write(base, 2, 0x0019)); // stop CLK, retain RST/ORRI
        assert_eq!(bank.device_reference_vectors().unwrap(), [VECTORS[channel]]);
        assert!(bank.device_write(base, 2, 9)); // ORRI off, preserve REF
        assert_eq!(bank.read(base + 3, 1), Some(2));
        assert!(bank.device_reference_vectors().unwrap().is_empty());
        assert!(!bank.pending(channel));
        assert_eq!(bank.fired(channel), 0);
        assert_eq!(bank.next_deadline(channel), None);
    }
    for base in BASES {
        assert!(bank.device_write(base, 2, 0x0019));
    }
    assert_eq!(bank.device_reference_vectors().unwrap(), VECTORS);
    assert_eq!(bank.device_reference_vectors().unwrap(), VECTORS);
}

#[test]
fn w1c_preserves_capture_and_reference_writes_do_not_acknowledge() {
    let mut bank = bank();
    let mut page = [0; 16];
    page[3] = 1; // test-only preexisting CAP; no capture event API
    page[12..16].copy_from_slice(&0x12345678_u32.to_be_bytes());
    assert!(bank.load_page(BASES[0], &page));
    assert!(bank.device_write(BASES[0], 2, 0x001b));
    bank.device_latch_reference(0).unwrap();
    assert!(bank.device_write(BASES[0] + 3, 1, 0));
    assert_eq!(bank.read(BASES[0] + 3, 1), Some(3));
    assert!(bank.device_write(BASES[0] + 4, 4, 0x43238));
    assert_eq!(bank.read(BASES[0] + 4, 4), Some(0x43238));
    assert_eq!(bank.read(BASES[0] + 3, 1), Some(3));
    assert!(bank.device_write(BASES[0] + 12, 4, u32::MAX));
    assert_eq!(bank.read(BASES[0] + 12, 4), Some(0));
    assert_eq!(bank.read(BASES[0] + 3, 1), Some(3));
    assert!(bank.device_write(BASES[0] + 3, 1, 2));
    assert_eq!(bank.read(BASES[0] + 3, 1), Some(1));
    assert!(bank.device_reference_vectors().unwrap().is_empty());
    bank.device_latch_reference(0).unwrap();
    assert!(bank.device_write(BASES[0] + 3, 1, 3));
    assert_eq!(bank.read(BASES[0] + 3, 1), Some(0));
    assert_eq!(bank.cleared(0), 0); // Oracle bookkeeping is separate
}

#[test]
fn unsupported_configuration_and_accesses_reject_atomically() {
    let mut bank = bank();
    assert!(bank.device_write(BASES[0], 2, 0x001b));
    bank.device_latch_reference(0).unwrap();
    for (offset, size, value) in [
        (0, 2, 0x001a), // RST 1 -> 0
        (0, 2, 0x001d), // live CLK bus/1 -> bus/16
        (0, 2, 0x005b), // capture enabled
        (2, 1, 0x20),
        (2, 1, 0x40),
        (2, 1, 0x10),
        (2, 1, 1),
        (2, 1, 2),
        (3, 1, 4), // reserved DTER bit
        (0, 4, 0),
        (1, 2, 0),
        (2, 2, 0),
        (3, 2, 0),
        (4, 2, 0),
        (8, 4, 0),
        (12, 2, 0),
        (0x3fff, 2, 0),
    ] {
        assert!(!bank.device_write(BASES[0] + offset, size, value));
        assert_eq!(bank.read(BASES[0], 4), Some(0x001b0002));
        assert_eq!(bank.read(BASES[0] + 4, 4), Some(u32::MAX));
    }
    assert_eq!(
        bank.device_latch_reference(4),
        Err(DeviceReferenceError::InvalidChannel { channel: 4 })
    );
    assert_eq!(bank.read(BASES[0], 4), Some(0x001b0002));
    assert!(bank.device_write(BASES[0], 2, 0x0019)); // CLK stop is supported
    assert!(!bank.device_write(BASES[0], 2, 0x001b)); // live restart unsupported
    assert_eq!(bank.read(BASES[0], 4), Some(0x00190002));
}

#[test]
fn externally_loaded_capture_or_advanced_modes_fail_closed() {
    for (dtmr, dtxmr) in [(0x40_u16, 0), (0, 0x20), (0, 0x40), (0, 0x10), (0, 1)] {
        let mut bank = bank();
        let mut page = [0; 16];
        page[..2].copy_from_slice(&dtmr.to_be_bytes());
        page[2] = dtxmr;
        page[3] = 1;
        assert!(bank.load_page(BASES[3], &page));
        let error = DeviceReferenceError::UnsupportedMode { channel: 3 };
        assert_eq!(bank.device_latch_reference(3), Err(error));
        assert_eq!(bank.device_reference_vectors(), Err(error));
        assert_eq!(bank.read(BASES[3] + 3, 1), Some(1));
        assert!(!bank.device_write(BASES[3] + 12, 4, 123));
    }
}

#[test]
fn oracle_defaults_and_plain_ram_writes_are_unchanged() {
    let mut oracle = DtimBank::default();
    assert_eq!(oracle.read(BASES[0] + 4, 4), Some(0));
    let device = bank();
    for base in BASES {
        assert_eq!(device.read(base + 4, 4), Some(u32::MAX));
    }
    assert!(oracle.write(BASES[0] + 3, 1, 3));
    assert_eq!(oracle.read(BASES[0] + 3, 1), Some(3));
    assert!(oracle.write(BASES[0] + 12, 4, 0x12345678));
    assert_eq!(oracle.read(BASES[0] + 12, 4), Some(0x12345678));
}
