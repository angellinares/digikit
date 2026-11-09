//! Bounded Device SIMR/CIMR byte access and preserved Oracle passthrough.
use machine::{Time, TimerPolicy};
use periph::{
    intc::{BASES, VECTOR_BASE},
    pit::F_BUS,
};

#[test]
fn device_mask_commands_control_candidate_eligibility_for_all_banks_and_halves() {
    for (bank, base) in BASES.into_iter().enumerate() {
        for source in [0_u32, 31, 32, 63] {
            let mut time = Time::new(TimerPolicy::Device, vec![], F_BUS);
            let force = base + if source < 32 { 0x14 } else { 0x10 };
            assert!(time.write(force, 4, 1 << (source % 32)));
            assert!(time.write(base + 0x40 + source, 1, 3));
            assert_eq!(
                time.device_event_candidate(false, false, false).unwrap(),
                None
            );
            assert!(time.write(base + 0x1d, 1, source));
            assert_eq!(
                time.device_event_candidate(false, false, false).unwrap(),
                Some((VECTOR_BASE[bank] + source as u16, 3))
            );
            assert_eq!(time.read(force, 4), Some(1 << (source % 32)));
            assert!(time.write(base + 0x1c, 1, source));
            assert_eq!(
                time.device_event_candidate(false, false, false).unwrap(),
                None
            );
            for other in BASES {
                assert_eq!(time.read(other + 8, 4), Some(u32::MAX));
                assert_eq!(time.read(other + 12, 4), Some(u32::MAX));
            }
        }
    }
}

#[test]
fn device_all_commands_ignore_field_and_command_reads_hide_stale_backing() {
    for base in BASES {
        for field in [0, 31, 32, 63] {
            let mut time = Time::new(TimerPolicy::Device, vec![], F_BUS);
            let mut page = [0; 32];
            page[0x1c] = 0xa5;
            page[0x1d] = 0x5a;
            assert!(time.load_page(base, &page));
            assert_eq!(time.read(base + 0x1c, 1), Some(0));
            assert_eq!(time.read(base + 0x1d, 1), Some(0));
            assert!(time.write(base + 0x1c, 1, 0x40 | field));
            assert_eq!(time.read(base + 8, 4), Some(u32::MAX));
            assert_eq!(time.read(base + 12, 4), Some(u32::MAX));
            assert!(time.write(base + 0x1d, 1, 0x40 | field));
            assert_eq!(time.read(base + 8, 4), Some(0));
            assert_eq!(time.read(base + 12, 4), Some(0));
            assert_eq!(time.read(base + 0x1c, 1), Some(0));
            assert_eq!(time.read(base + 0x1d, 1), Some(0));
            for other in BASES.into_iter().filter(|other| *other != base) {
                assert_eq!(time.read(other + 8, 4), Some(u32::MAX));
                assert_eq!(time.read(other + 12, 4), Some(u32::MAX));
            }
        }
    }
}

#[test]
fn device_rejects_reserved_wrong_width_and_overlapping_accesses_without_mutation() {
    for base in BASES {
        let mut time = Time::new(TimerPolicy::Device, vec![], F_BUS);
        let mut page = [0; 32];
        page[8..16].copy_from_slice(&[0x12, 0x34, 0x56, 0x78, 0x89, 0xab, 0xcd, 0xef]);
        page[0x1a..0x20].copy_from_slice(&[0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc]);
        assert!(time.load_page(base, &page));
        let mut before = periph::snap::Writer::new();
        time.snap_save(&mut before);
        for (offset, size) in [
            (0x1a, 4),
            (0x1b, 2),
            (0x1b, 4),
            (0x1c, 0),
            (0x1c, 2),
            (0x1c, 4),
            (0x1d, 0),
            (0x1d, 2),
            (0x1d, 4),
        ] {
            assert_eq!(time.read(base + offset, size), None);
            assert!(!time.write(base + offset, size, u32::MAX));
        }
        for offset in [0x1c, 0x1d] {
            for value in [0x80, 0xbf, 0xc0, 0xff] {
                assert!(!time.write(base + offset, 1, value));
            }
        }
        let mut after = periph::snap::Writer::new();
        time.snap_save(&mut after);
        assert_eq!(after.buf, before.buf);
        assert_eq!(time.read(base + 0x1a, 2), Some(0x1234));
        assert_eq!(time.read(base + 0x1e, 2), Some(0x9abc));
    }
}

#[test]
fn oracle_mask_commands_remain_raw_bytes_and_overlapping_passthrough() {
    for base in BASES {
        let mut time = Time::new(TimerPolicy::Oracle, vec![], F_BUS);
        assert!(time.write(base + 0x1c, 1, 0xff));
        assert!(time.write(base + 0x1d, 1, 0xa5));
        assert_eq!(time.read(base + 0x1c, 1), Some(0xff));
        assert_eq!(time.read(base + 0x1d, 1), Some(0xa5));
        assert_eq!(time.read(base + 8, 4), Some(0));
        assert_eq!(time.read(base + 12, 4), Some(0));
        assert!(time.write(base + 0x1b, 4, 0xdeadbeef));
        assert_eq!(time.read(base + 0x1b, 4), Some(0xdeadbeef));
    }
}
