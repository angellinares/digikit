use coldfire::Bus;
use emmc_card::{Card, DEFAULT_CAPACITY_BLOCKS};
use machine::{Board, CompletionPolicy, SemaphoreAddresses, Time, TimeError, TimerPolicy};
use periph::{
    intc::{BASES as INTC_BASES, VECTOR_BASE},
    pit::{BASES, F_BUS, VECTORS},
};

fn board() -> Board {
    Board::new(
        Card::new(DEFAULT_CAPACITY_BLOCKS).unwrap(),
        SemaphoreAddresses::default(),
        CompletionPolicy::Oracle,
    )
}

#[test]
fn unattached_timer_is_absent() {
    let mut board = board();
    assert!(board.time_mut().is_none());
    assert!(board.take_time().is_none());
}

#[test]
fn timer_allocation_and_mmio_survive_round_trips() {
    let mut board = board();
    board.attach_time(Time::new(TimerPolicy::Oracle, vec![0], F_BUS));
    let original_ptr = std::ptr::from_ref::<Time>(board.time_mut().unwrap());

    for round in 0..4u32 {
        board.write16(BASES[0] + 2, (131 + round) as u16).unwrap();
        let mut timer = board.take_time().unwrap();
        assert_eq!(std::ptr::from_ref::<Time>(&*timer), original_ptr);
        assert!(board.time_mut().is_none());
        assert!(board.take_time().is_none());
        assert!(board.read16(BASES[0] + 2).is_err());
        assert!(board.write16(BASES[0] + 2, 0).is_err());
        assert_eq!(timer.read(BASES[0] + 2, 2), Some(131 + round));
        assert!(timer.write(BASES[0] + 2, 2, 140 + round));
        board.restore_time(timer);
        assert_eq!(
            std::ptr::from_ref::<Time>(board.time_mut().unwrap()),
            original_ptr
        );
        assert_eq!(board.read16(BASES[0] + 2).unwrap(), (140 + round) as u16);
    }
}

#[test]
fn pending_pit_state_survives_detachment() {
    let mut board = board();
    board.attach_time(Time::new(TimerPolicy::Oracle, vec![0], F_BUS));
    let time = board.time_mut().unwrap();
    assert!(time.write(BASES[0], 2, 0x000b));
    assert!(time.write(BASES[0] + 2, 2, 131));
    let vector = VECTORS[0];
    let source = vector - VECTOR_BASE[2];
    assert!(time.write(INTC_BASES[2] + 0x40 + u32::from(source), 1, 1));
    assert_eq!(time.deadline(0), Some(132));
    time.seed_sr(0x2100);
    assert_eq!(time.service(132).unwrap(), []);

    let mut timer = board.take_time().unwrap();
    assert!(board.time_mut().is_none());
    assert_eq!(timer.policy(), TimerPolicy::Oracle);
    assert_eq!(timer.read(BASES[0] + 2, 2), Some(131));
    timer.seed_sr(0x2000);
    assert_eq!(timer.service(132).unwrap(), vec![(VECTORS[0], 1)]);
    board.restore_time(timer);
    let time = board.time_mut().unwrap();
    assert_eq!(time.deadline(132), Some(264));
    assert_eq!(time.service(132).unwrap(), []);
}

#[test]
fn device_error_does_not_change_box_ownership() {
    let mut board = board();
    board.attach_time(Time::new(TimerPolicy::Device, vec![], F_BUS));
    let original_ptr = std::ptr::from_ref::<Time>(board.time_mut().unwrap());
    let mut timer = board.take_time().unwrap();
    assert!(board.time_mut().is_none());
    assert_eq!(
        timer.service(0),
        Err(TimeError::DeviceInterruptDeliveryUnsupported)
    );
    board.restore_time(timer);
    let time = board.time_mut().unwrap();
    assert_eq!(time.policy(), TimerPolicy::Device);
    assert_eq!(std::ptr::from_ref::<Time>(time), original_ptr);
}
