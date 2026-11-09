//! Bounded Device PIT event facade and real CPU entry, without automatic arbitration.
use coldfire::{Bus, Cpu, InterruptPolicy};
use emmc_card::{Card, CardIdentity};
use machine::{
    Board, CompletionPolicy, DevicePitError, SemaphoreAddresses, Time, TimeError, TimerPolicy,
};
use periph::{
    intc,
    pit::{BASES, F_BUS, VECTORS},
};

#[test]
fn facade_checks_policy_and_channel_and_keeps_raw_requests_mask_independent() {
    let mut oracle = Time::new(TimerPolicy::Oracle, vec![0], F_BUS);
    assert!(oracle.write(BASES[0], 2, 0x000f));
    assert_eq!(
        oracle.device_pit_timeout(0),
        Err(DevicePitError::DevicePolicyRequired)
    );
    assert_eq!(
        oracle.device_pit_requests(),
        Err(DevicePitError::DevicePolicyRequired)
    );
    assert_eq!(oracle.read(BASES[0], 2), Some(0x000f));
    assert!(oracle.write(BASES[0] + 2, 2, 20));
    assert_eq!(oracle.read(BASES[0], 2), Some(0x000f));
    let mut time = Time::new(TimerPolicy::Device, vec![], F_BUS);
    for (channel, base) in BASES.into_iter().enumerate() {
        assert!(time.write(base, 2, 8));
        time.device_pit_timeout(channel).unwrap();
    }
    time.seed_sr(0x2700);
    assert_eq!(time.read(intc::BASES[2] + 0x0c, 4), Some(u32::MAX));
    assert_eq!(time.device_pit_requests().unwrap(), VECTORS);
    assert_eq!(time.device_pit_requests().unwrap(), VECTORS);
    assert_eq!(
        time.device_pit_timeout(4),
        Err(DevicePitError::InvalidChannel { channel: 4 })
    );
    for (addr, size) in [
        (BASES[0], 1),
        (BASES[0], 4),
        (BASES[0] + 1, 2),
        (BASES[0] + 4, 2),
    ] {
        assert!(!time.write(addr, size, u32::MAX));
        assert_eq!(time.read(BASES[0], 4), Some(0x000c0000));
    }
    assert_eq!(
        time.service(1000),
        Err(TimeError::DeviceInterruptDeliveryUnsupported)
    );
    assert_eq!(time.device_pit_requests().unwrap(), VECTORS);
}

#[test]
fn cpu_entry_preserves_pif_until_guest_word_acknowledgement() {
    const RAM: u32 = 0x4000_0000;
    const HANDLER: u32 = RAM + 0x2000;
    let card = Card::with_backing(CardIdentity::Mlc4G, None).unwrap();
    let mut board = Board::new(
        card,
        SemaphoreAddresses::default(),
        CompletionPolicy::Device,
    );
    board.map_ram_page(RAM).unwrap();
    board.attach_time(Time::new(TimerPolicy::Device, vec![], F_BUS));
    board.write16(BASES[0], 8).unwrap();
    board.time_mut().unwrap().device_pit_timeout(0).unwrap();
    board
        .write32(RAM + u32::from(VECTORS[0]) * 4, HANDLER)
        .unwrap();
    // MOVE.W D0,PCSR0, using the actual scheduler's acknowledgement value.
    board.write16(HANDLER, 0x33c0).unwrap();
    board.write32(HANDLER + 2, BASES[0]).unwrap();
    board.write16(HANDLER + 6, 0x4e73).unwrap(); // RTE
    let mut cpu = Cpu::new();
    cpu.d[0] = 0x053f;
    cpu.pc = RAM + 0x1000;
    cpu.sr = 0x2000;
    cpu.a[7] = RAM + 0x8000;
    cpu.ctrl.vbr = RAM;
    let before = (cpu.pc, cpu.sr, cpu.a[7]);
    // Explicit controlled entry: storage arbitration still excludes PIT.
    cpu.take_interrupt(
        &mut board,
        VECTORS[0] as u8,
        Some(3),
        InterruptPolicy::Device,
    )
    .unwrap();
    assert_eq!(cpu.pc, HANDLER);
    assert_eq!(board.read16(BASES[0]).unwrap(), 12);
    assert_eq!(
        board.time().unwrap().device_pit_requests().unwrap(),
        [VECTORS[0]]
    );
    cpu.step(&mut board).unwrap();
    assert_eq!(cpu.last_exception, None);
    assert_eq!(board.read16(BASES[0]).unwrap(), 0x053b);
    assert!(
        board
            .time()
            .unwrap()
            .device_pit_requests()
            .unwrap()
            .is_empty()
    );
    cpu.step(&mut board).unwrap();
    assert_eq!((cpu.pc, cpu.sr, cpu.a[7]), before);
}
