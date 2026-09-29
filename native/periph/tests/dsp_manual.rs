//! Unit tests for `dsp::Fifo`, a port of `emu/dsp.py`'s FlexBus coprocessor
//! port model -- no manual chapter covers this custom port (the vendor's
//! own protocol, not documented in the MCF5441XRM), so these tests check
//! the port against `emu/dsp.py`'s own documented behaviour instead.

use periph::dsp::{Fifo, LATCH, READY, STATUS};

#[test]
fn addresses_match_the_python_model() {
    assert_eq!(periph::dsp::BASE, 0x8C00_0000);
    assert_eq!(STATUS, 0x8C00_0002);
    assert_eq!(LATCH, 0x8C00_000A);
}

#[test]
fn poll_delay_zero_is_always_ready() {
    // `emu/dsp.py`: `ready = READY if self.polls > self.poll_delay else 0`,
    // and `polls` is incremented before the check -- at the default
    // `poll_delay=0` (every trace `tools/mmio_record.py` records uses),
    // `polls` is always >= 1 immediately, so every read is READY.
    let mut fifo = Fifo::new(0);
    for _ in 0..5 {
        assert_eq!(fifo.read_status(STATUS), Some(READY));
    }
}

#[test]
fn nonzero_poll_delay_holds_ready_until_enough_polls() {
    let mut fifo = Fifo::new(3);
    assert_eq!(fifo.read_status(STATUS), Some(0)); // poll 1
    assert_eq!(fifo.read_status(STATUS), Some(0)); // poll 2
    assert_eq!(fifo.read_status(STATUS), Some(0)); // poll 3
    assert_eq!(fifo.read_status(STATUS), Some(READY)); // poll 4 > delay 3
}

#[test]
fn a_write_resets_the_poll_count() {
    let mut fifo = Fifo::new(5);
    fifo.read_status(STATUS);
    fifo.read_status(STATUS);
    assert!(fifo.write(STATUS));
    assert_eq!(fifo.polls, 0);
    assert_eq!(fifo.words, 1);
}

#[test]
fn latch_write_counts_a_burst_only() {
    let mut fifo = Fifo::new(0);
    assert!(fifo.write(LATCH));
    assert_eq!(fifo.bursts, 1);
    assert_eq!(fifo.words, 0);
}

#[test]
fn addresses_outside_status_and_latch_are_not_owned() {
    let mut fifo = Fifo::new(0);
    assert_eq!(fifo.read_status(periph::dsp::BASE), None);
    assert!(!fifo.write(periph::dsp::BASE + 4));
    assert!(!Fifo::owns(periph::dsp::BASE + 4));
}
