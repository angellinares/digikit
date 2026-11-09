//! DMA timer 0's counter as a free-running bus clock (`Board::enable_dtcn0`).

use coldfire::Bus;
use emmc_card::{Card, CardIdentity};
use machine::{Board, CompletionPolicy, DTCN0, SemaphoreAddresses, Time, TimerPolicy};

fn board() -> Board {
    let mut b = Board::new(
        Card::new(CardIdentity::Mlc4G).unwrap(),
        SemaphoreAddresses::default(),
        CompletionPolicy::Oracle,
    );
    b.attach_time(Time::with_dtims(
        TimerPolicy::Oracle,
        vec![3, 2, 0],
        vec![3, 1],
        periph::clock::cf_ips() as f64,
    ));
    b
}

#[test]
fn dtcn0_counts_the_bus_clock_from_the_instruction_count() {
    let mut b = board();
    b.enable_dtcn0();
    b.set_clock(0);
    assert_eq!(b.read32(DTCN0), Ok(0));
    let ips = periph::clock::cf_ips();
    b.set_clock(ips / 1000); // 1 ms of emulated time
    assert_eq!(b.read32(DTCN0), Ok(132_000));
    b.set_clock(ips * 40); // past 2^32 ticks: the counter wraps
    assert_eq!(
        b.read32(DTCN0),
        Ok((132_000_000u64 * 40 % (1 << 32)) as u32)
    );
}

#[test]
fn without_the_model_dtcn0_is_a_plain_register() {
    let mut b = board();
    b.set_clock(periph::clock::cf_ips());
    assert_eq!(b.read32(DTCN0), Ok(0));
}
