use coldfire::{Bus, Cpu, InterruptPolicy};
use emmc_card::{Card, CardIdentity};
use machine::{Board, CompletionPolicy, DeviceDtimError, SemaphoreAddresses, Time, TimerPolicy};
use periph::{
    dtim::{BASES, F_BUS, VECTORS},
    intc,
};

#[test]
fn facade_requires_device_policy_and_preserves_registers_on_errors() {
    let mut oracle = Time::new(TimerPolicy::Oracle, vec![], F_BUS);
    assert_eq!(oracle.read(BASES[0] + 4, 4), Some(0));
    assert_eq!(
        oracle.device_dtim_reference(0),
        Err(DeviceDtimError::DevicePolicyRequired)
    );
    assert_eq!(
        oracle.device_dtim_reference_requests(),
        Err(DeviceDtimError::DevicePolicyRequired)
    );
    let mut time = Time::with_dtims(TimerPolicy::Device, vec![], vec![], F_BUS);
    assert_eq!(time.read(BASES[0] + 4, 4), Some(u32::MAX));
    assert_eq!(
        time.device_dtim_reference(4),
        Err(DeviceDtimError::InvalidChannel { channel: 4 })
    );
    assert_eq!(time.read(BASES[0] + 3, 1), Some(0));
    assert!(time.write(BASES[0], 2, 0x001d));
    time.device_dtim_reference(0).unwrap();
    for (offset, size, value) in [
        (0, 2, 0x001c),
        (0, 2, 0x001b),
        (2, 1, 0x20),
        (3, 2, 0),
        (8, 4, 0),
    ] {
        assert!(!time.write(BASES[0] + offset, size, value));
        assert_eq!(time.read(BASES[0], 4), Some(0x001d0002));
    }
    let mut page = [0; 16];
    page[2] = 0x20;
    assert!(time.load_page(BASES[2], &page));
    let error = DeviceDtimError::UnsupportedMode { channel: 2 };
    assert_eq!(time.device_dtim_reference(2), Err(error));
    assert_eq!(time.device_dtim_reference_requests(), Err(error));
    assert_eq!(time.read(BASES[2] + 3, 1), Some(0));
}

#[test]
fn reference_requests_ignore_masks_and_ipl_without_entry_consumption() {
    const RAM: u32 = 0x40000000;
    let card = Card::with_backing(CardIdentity::Mlc4G, None).unwrap();
    let mut board = Board::new(
        card,
        SemaphoreAddresses::default(),
        CompletionPolicy::Device,
    );
    board.map_ram_page(RAM).unwrap();
    board.attach_time(Time::with_dtims(TimerPolicy::Device, vec![], vec![], F_BUS));
    board.write16(BASES[3], 0x001d).unwrap();
    board.time_mut().unwrap().device_dtim_reference(3).unwrap();
    board.time_mut().unwrap().seed_sr(0x2700);
    assert_eq!(board.read32(intc::BASES[0] + 8).unwrap(), u32::MAX);
    assert_eq!(
        board
            .time()
            .unwrap()
            .device_dtim_reference_requests()
            .unwrap(),
        [VECTORS[3]]
    );
    board
        .write32(RAM + u32::from(VECTORS[3]) * 4, RAM + 0x2000)
        .unwrap();
    board.write16(RAM + 0x2000, 0x4e73).unwrap();
    let mut cpu = Cpu::new();
    cpu.pc = RAM + 0x1000;
    cpu.sr = 0x2000;
    cpu.a[7] = RAM + 0x8000;
    cpu.ctrl.vbr = RAM;
    cpu.take_interrupt(
        &mut board,
        VECTORS[3] as u8,
        Some(2),
        InterruptPolicy::Device,
    )
    .unwrap();
    cpu.step(&mut board).unwrap();
    assert_eq!(cpu.pc, RAM + 0x1000);
    assert_eq!(board.read8(BASES[3] + 3).unwrap(), 2);
    assert_eq!(
        board
            .time()
            .unwrap()
            .device_dtim_reference_requests()
            .unwrap(),
        [VECTORS[3]]
    );
    board.write8(BASES[3] + 3, 2).unwrap();
    assert!(
        board
            .time()
            .unwrap()
            .device_dtim_reference_requests()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn supplied_post_prescaler_ticks_latch_without_delivery_or_host_writes() {
    let mut time = Time::with_dtims(TimerPolicy::Device, vec![], vec![], F_BUS);
    let base = BASES[3];
    assert!(time.write(base + 4, 4, 2));
    assert!(time.write(base, 2, 0xff0d)); // ORRI off, PS and divider already applied by caller
    time.device_dtim_advance_ticks(3, 2).unwrap();
    assert_eq!(time.read(base + 12, 4), Some(2));
    assert_eq!(time.read(base + 3, 1), Some(0));
    time.device_dtim_advance_ticks(3, 1).unwrap();
    assert_eq!(time.read(base + 12, 4), Some(0));
    assert_eq!(time.read(base + 3, 1), Some(2));
    assert!(time.device_dtim_reference_requests().unwrap().is_empty());
    assert!(time.write(base, 2, 0xff1d));
    assert_eq!(time.device_dtim_reference_requests().unwrap(), [VECTORS[3]]);
    assert!(time.write(intc::BASES[0] + 0x40 + 35, 1, 2));
    assert!(time.write(intc::BASES[0] + 0x1d, 1, 35));
    time.seed_sr(0x2700);
    assert_eq!(
        time.device_event_candidate(false, false, false).unwrap(),
        Some((VECTORS[3], 2))
    );
    assert!(time.take_host_writes().is_empty());
    assert!(time.write(base + 3, 1, 2));
    time.device_dtim_advance_ticks(3, 6).unwrap();
    assert_eq!(time.read(base + 3, 1), Some(2)); // two periods, one sticky event
    assert_eq!(time.read(base + 12, 4), Some(0));
}

#[test]
fn supplied_tick_errors_preserve_time_and_oracle_remains_unchanged() {
    let mut oracle = Time::new(TimerPolicy::Oracle, vec![], F_BUS);
    assert_eq!(
        oracle.device_dtim_advance_ticks(0, 1),
        Err(DeviceDtimError::DevicePolicyRequired)
    );
    assert_eq!(oracle.read(BASES[0] + 12, 4), Some(0));
    assert_eq!(oracle.read(BASES[0] + 3, 1), Some(0));
    let mut time = Time::new(TimerPolicy::Device, vec![], F_BUS);
    let mut page = [0; 16];
    page[..2].copy_from_slice(&0x001d_u16.to_be_bytes());
    page[3] = 1;
    page[4..8].copy_from_slice(&2_u32.to_be_bytes());
    page[12..16].copy_from_slice(&3_u32.to_be_bytes());
    assert!(time.load_page(BASES[0], &page));
    let mut before = periph::snap::Writer::new();
    time.snap_save(&mut before);
    assert_eq!(
        time.device_dtim_advance_ticks(0, u64::MAX),
        Err(DeviceDtimError::CounterOutOfRange {
            channel: 0,
            counter: 3,
            reference: 2
        })
    );
    assert_eq!(
        time.device_dtim_advance_ticks(4, 1),
        Err(DeviceDtimError::InvalidChannel { channel: 4 })
    );
    let mut after = periph::snap::Writer::new();
    time.snap_save(&mut after);
    assert_eq!(after.buf, before.buf);
    assert!(time.take_host_writes().is_empty());
}

#[test]
fn unsupported_tick_mode_is_not_bypassed_by_zero_ticks() {
    let mut time = Time::new(TimerPolicy::Device, vec![], F_BUS);
    let mut page = [0; 16];
    page[..2].copy_from_slice(&0x001d_u16.to_be_bytes());
    page[2] = 0x80;
    page[3] = 1;
    assert!(time.load_page(BASES[0], &page));
    let mut before = periph::snap::Writer::new();
    time.snap_save(&mut before);
    assert_eq!(
        time.device_dtim_advance_ticks(0, 0),
        Err(DeviceDtimError::UnsupportedMode { channel: 0 })
    );
    let mut after = periph::snap::Writer::new();
    time.snap_save(&mut after);
    assert_eq!(after.buf, before.buf);
}
