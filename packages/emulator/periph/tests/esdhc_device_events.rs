//! Bounded Device-policy interrupt state; no CPU or DMA delivery is implied.

use periph::{
    esdhc::{self, CardPort, DeviceEventError, Esdhc, RegisterPolicy},
    snap::{Reader, Writer},
};

const CC: u32 = 1;
const TC: u32 = 2;
const BWR: u32 = 1 << 4;
const BRR: u32 = 1 << 5;
const CTOE: u32 = 1 << 16;
const DTOE: u32 = 1 << 20;
const DCE: u32 = 1 << 21;

struct Card;
impl CardPort for Card {
    fn command(&mut self, idx: u8, arg: u32) -> [u32; 4] {
        [u32::from(idx), arg, 3, 4]
    }
    fn read_word(&mut self, _idx: u8, pattern: u32) -> u32 {
        !pattern
    }
}

fn device() -> Esdhc<Card> {
    Esdhc::with_policy(Card, RegisterPolicy::Device)
}
fn write(h: &mut Esdhc<Card>, off: u32, value: u32) {
    assert!(h.write(esdhc::BASE + off, 4, value));
}
fn read(h: &mut Esdhc<Card>, off: u32) -> u32 {
    h.read(esdhc::BASE + off, 4).unwrap()
}
fn host_writes(h: &mut Esdhc<Card>) -> Vec<(u32, [u8; 4])> {
    std::iter::from_fn(|| h.take_host_write()).collect()
}

#[test]
fn disabled_status_event_is_not_retained_for_later_enable() {
    let mut h = device();
    write(&mut h, esdhc::IRQSTATEN, 0);
    write(&mut h, esdhc::IRQSIGEN, CC | DTOE);
    h.device_event(CC | DTOE).unwrap();
    assert_eq!(read(&mut h, esdhc::IRQSTAT), 0);
    write(&mut h, esdhc::IRQSTATEN, CC | DTOE);
    assert_eq!(read(&mut h, esdhc::IRQSTAT), 0);
    assert!(!h.interrupt_pending());
    h.device_event(DTOE).unwrap();
    assert!(h.interrupt_pending());
}

#[test]
fn signal_mask_preserves_status_and_unmask_asserts_shared_line() {
    let mut h = device();
    h.device_event(CC | DCE).unwrap();
    assert_eq!(read(&mut h, esdhc::IRQSTAT), CC | DCE);
    assert!(!h.interrupt_pending());
    write(&mut h, esdhc::IRQSIGEN, TC); // unrelated enabled source
    assert!(!h.interrupt_pending());
    write(&mut h, esdhc::IRQSIGEN, DCE);
    assert!(h.interrupt_pending());
    write(&mut h, esdhc::IRQSIGEN, 0);
    assert!(!h.interrupt_pending());
    assert_eq!(read(&mut h, esdhc::IRQSTAT), CC | DCE);
}

#[test]
fn clearing_status_enable_clears_only_its_latched_status() {
    let mut h = device();
    h.device_event(CC | TC | CTOE).unwrap();
    write(&mut h, esdhc::IRQSIGEN, CC);
    assert!(h.interrupt_pending());
    write(&mut h, esdhc::IRQSTATEN, TC | CTOE);
    assert_eq!(read(&mut h, esdhc::IRQSTAT), TC | CTOE);
    assert!(!h.interrupt_pending());
    write(&mut h, esdhc::IRQSTATEN, CC | TC | CTOE);
    assert_eq!(read(&mut h, esdhc::IRQSTAT), TC | CTOE);
}

#[test]
fn error_and_completion_are_independent_raw_bits_with_partial_w1c() {
    let mut h = device();
    h.device_event(TC | DTOE | DCE).unwrap();
    write(&mut h, esdhc::IRQSIGEN, TC | DTOE);
    write(&mut h, esdhc::IRQSTAT, DTOE);
    assert_eq!(read(&mut h, esdhc::IRQSTAT), TC | DCE);
    assert!(h.interrupt_pending());
    write(&mut h, esdhc::IRQSTAT, TC);
    assert_eq!(read(&mut h, esdhc::IRQSTAT), DCE);
    assert!(!h.interrupt_pending());
}

#[test]
fn byte_and_halfword_writes_use_big_endian_register_lanes() {
    let mut h = device();
    h.device_event(CC | TC | CTOE | DCE).unwrap();
    assert!(h.write(esdhc::BASE + esdhc::IRQSTAT + 3, 1, CC));
    assert_eq!(read(&mut h, esdhc::IRQSTAT), TC | CTOE | DCE);
    assert!(h.write(esdhc::BASE + esdhc::IRQSTAT, 2, 0x20));
    assert_eq!(read(&mut h, esdhc::IRQSTAT), TC | CTOE);
    // Top halfword clears CTOESEN but retains data CRC enable.
    assert!(h.write(esdhc::BASE + esdhc::IRQSTATEN, 2, 0x20));
    assert_eq!(read(&mut h, esdhc::IRQSTAT), TC);
    assert!(h.write(esdhc::BASE + esdhc::IRQSIGEN + 3, 1, TC));
    assert!(h.interrupt_pending());
    assert!(h.write(esdhc::BASE + esdhc::IRQSTATEN + 3, 1, CC));
    assert_eq!(read(&mut h, esdhc::IRQSTAT), 0);
    assert!(!h.interrupt_pending());
}

#[test]
fn invalid_event_is_atomic_and_oracle_rejects_host_event_input() {
    let mut h = device();
    h.device_event(CC).unwrap();
    let before = host_writes(&mut h);
    assert_eq!(before.len(), 1);
    for bit in [
        1 << 2,
        1 << 3,
        1 << 6,
        1 << 8,
        1 << 17,
        1 << 24,
        1 << 28,
        1 << 31,
    ] {
        assert_eq!(
            h.device_event(TC | bit),
            Err(DeviceEventError::UnsupportedStatusBits(bit))
        );
        assert_eq!(read(&mut h, esdhc::IRQSTAT), CC);
        assert!(host_writes(&mut h).is_empty());
    }
    let mut oracle = Esdhc::new(Card);
    assert_eq!(
        oracle.device_event(CC),
        Err(DeviceEventError::DevicePolicyRequired)
    );
    assert_eq!(read(&mut oracle, esdhc::IRQSTAT), 0);
    assert!(host_writes(&mut oracle).is_empty());
}

#[test]
fn device_commands_signal_command_and_ready_but_require_transfer_event() {
    for (xfer, ready) in [
        (0, 0),
        (0x0E3A_0010, BRR),
        (0x1922_0000, BWR),
        (0x0C03_0000, 0),
    ] {
        let mut h = device();
        write(&mut h, esdhc::IRQSIGEN, CC | TC | BRR | BWR);
        write(&mut h, esdhc::XFERTYP, xfer);
        assert_eq!(read(&mut h, esdhc::IRQSTAT), CC | ready);
        assert!(h.interrupt_pending());
        write(&mut h, esdhc::IRQSTAT, CC | ready);
        assert!(!h.interrupt_pending());
        h.device_event(TC).unwrap();
        assert_eq!(read(&mut h, esdhc::IRQSTAT), TC);
        assert!(h.interrupt_pending());
    }
    let mut masked = device();
    write(&mut masked, esdhc::IRQSTATEN, 0);
    write(&mut masked, esdhc::XFERTYP, 0x0E3A_0010);
    assert_eq!(read(&mut masked, esdhc::IRQSTAT), 0);
    assert_ne!(read(&mut masked, esdhc::PRSSTAT) & (1 << 11), 0);
    assert_eq!(read(&mut masked, esdhc::DATPORT), u32::MAX);
}

#[test]
fn command_and_data_reset_clear_their_status_without_clearing_errors_or_enables() {
    let mut h = device();
    h.device_event(CC | TC | BWR | BRR | CTOE | DTOE).unwrap();
    write(&mut h, esdhc::IRQSIGEN, CC | TC | CTOE | DTOE);
    // RSTC: RM Table 25-13 clears CC, not the error bits.
    write(&mut h, esdhc::SYSCTL, 1 << 25);
    assert_eq!(read(&mut h, esdhc::IRQSTAT), TC | BWR | BRR | CTOE | DTOE);
    assert_eq!(read(&mut h, esdhc::SYSCTL) & (1 << 25), 0);
    // RSTD clears transfer, buffer-ready, block-gap and internal-DMA status.
    // A restored page can contain the two event families not generated here.
    let mut page = vec![0; esdhc::SIZE as usize];
    let mut w = Writer::new();
    h.snap_save(&mut w);
    page.copy_from_slice(&w.buf[..esdhc::SIZE as usize]);
    page[esdhc::IRQSTAT as usize + 3] |= (1 << 2) | (1 << 3);
    assert!(h.load_page(esdhc::BASE, &page));
    h.device_event(CC).unwrap();
    write(&mut h, esdhc::SYSCTL, 1 << 26);
    assert_eq!(read(&mut h, esdhc::IRQSTAT), CC | CTOE | DTOE);
    assert_eq!(read(&mut h, esdhc::IRQSTATEN), 0x117F_013F);
    assert_eq!(read(&mut h, esdhc::IRQSIGEN), CC | TC | CTOE | DTOE);
    assert!(h.interrupt_pending());
}

#[test]
fn rsta_sets_status_enables_to_reset_value_so_completion_latches() {
    let mut h = device();
    write(&mut h, esdhc::IRQSTATEN, 0);
    write(&mut h, esdhc::SYSCTL, 1 << 24);
    assert_eq!(read(&mut h, esdhc::IRQSTATEN), 0x117F_013F);
    assert_eq!(read(&mut h, esdhc::SYSCTL) & (1 << 24), 0);
    h.device_event(CC | TC).unwrap();
    assert_eq!(read(&mut h, esdhc::IRQSTAT), CC | TC);
}

#[test]
fn snapshot_restores_masked_status_and_host_write_queue_without_new_state() {
    let mut h = device();
    h.device_event(TC | DTOE).unwrap();
    let mut saved = Writer::new();
    h.snap_save(&mut saved);
    // Existing format: raw register page, pattern, host count, address/value.
    assert_eq!(saved.buf.len(), esdhc::SIZE as usize + 4 + 8 + 8);
    let mut restored = device();
    let mut reader = Reader::new(&saved.buf);
    restored.snap_load(&mut reader).unwrap();
    assert!(reader.is_empty());
    assert_eq!(read(&mut restored, esdhc::IRQSTAT), TC | DTOE);
    assert!(!restored.interrupt_pending());
    assert_eq!(host_writes(&mut restored), host_writes(&mut h));
    write(&mut restored, esdhc::IRQSIGEN, DTOE);
    assert!(restored.interrupt_pending());
}

#[test]
fn oracle_retains_status_stores_command_bits_and_exact_host_write_order() {
    let mut h = Esdhc::new(Card);
    write(&mut h, esdhc::IRQSTAT, DCE);
    write(&mut h, esdhc::IRQSTATEN, 0);
    write(&mut h, esdhc::IRQSIGEN, u32::MAX);
    write(&mut h, esdhc::CMDARG, 0x1234_0000);
    write(&mut h, esdhc::XFERTYP, 0x0E3A_0010);
    let expected = [
        (esdhc::CMDRSP0, 14),
        (esdhc::CMDRSP1, 0x1234_0000),
        (esdhc::CMDRSP2, 3),
        (esdhc::CMDRSP3, 4),
        (esdhc::PRSSTAT, 0xFF89_00F8),
        (esdhc::IRQSTAT, DCE | CC | TC),
        (esdhc::PRSSTAT, 0xFF89_08F8),
        (esdhc::IRQSTAT, DCE | CC | TC | BRR),
        (esdhc::DATPORT, u32::MAX),
    ];
    assert_eq!(
        host_writes(&mut h),
        expected.map(|(off, value)| (esdhc::BASE + off, value.to_be_bytes()))
    );
    assert!(!h.interrupt_pending());
    write(&mut h, esdhc::IRQSTAT, TC);
    write(&mut h, esdhc::SYSCTL, (1 << 25) | (1 << 26));
    assert_eq!(read(&mut h, esdhc::IRQSTAT), TC);
}

#[test]
fn crossing_interrupt_register_writes_are_rejected_before_mutation() {
    let mut h = device();
    h.device_event(CC | DTOE).unwrap();
    let mut before = Writer::new();
    h.snap_save(&mut before);
    for (off, size) in [
        (esdhc::SYSCTL - 1, 2),
        (esdhc::SYSCTL + 3, 2),
        (esdhc::IRQSTAT + 2, 4),
        (esdhc::IRQSTATEN + 3, 2),
        (esdhc::IRQSIGEN + 1, 4),
    ] {
        assert!(!h.write(esdhc::BASE + off, size, u32::MAX));
        let mut after = Writer::new();
        h.snap_save(&mut after);
        assert_eq!(after.buf, before.buf);
    }
}
