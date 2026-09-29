//! SD continuity-gate tests, ported from `emu/gpio.py` and RM chapter 15.

use periph::gpio::{GPIO_BASE, PCLRR_D, PPDSDR_C, PPDSDR_D, SdGate};
use periph::regfile::SLOT_SIZE;

#[test]
fn drive_high_then_clear_low_changes_only_the_sensed_bit() {
    let mut gate = SdGate::default();
    gate.write(PPDSDR_D, 1, 0x10); // PPDSDR: one sets D4.
    assert_eq!(gate.sense(), 0x08);
    gate.write(PCLRR_D, 1, 0xef); // PCLRR: zero clears D4.
    assert_eq!(gate.sense(), 0x00);
}

#[test]
fn sense_retains_unrelated_port_c_bits() {
    let mut gate = SdGate::default();
    let mut page = vec![0; SLOT_SIZE];
    page[(PPDSDR_C - GPIO_BASE) as usize] = 0xa5;
    assert!(gate.load_page(GPIO_BASE, &page));

    gate.write(PPDSDR_D, 1, 0x10);
    assert_eq!(gate.sense(), 0xad);
    gate.write(PCLRR_D, 1, 0xef);
    assert_eq!(gate.sense(), 0xa5);
}

#[test]
fn sensed_pin_is_byte_only() {
    let gate = SdGate::default();
    assert_eq!(gate.read(PPDSDR_C, 1), Some(0));
    assert_eq!(gate.read(PPDSDR_C, 2), None);
}

#[test]
fn reset_and_checkpoint_state_seed_the_hook_separately_from_raw_bytes() {
    let mut gate = SdGate::default();
    gate.write(PPDSDR_D, 1, 0x10);
    assert_eq!(gate.sense(), 0x08);

    let mut page = vec![0; SLOT_SIZE];
    page[(PPDSDR_C - GPIO_BASE) as usize] = 0x40;
    gate.load_page(GPIO_BASE, &page);
    gate.load_state(false);
    assert_eq!(gate.sense(), 0x40);

    gate.load_state(true);
    assert_eq!(gate.sense(), 0x48);
    assert_eq!(SdGate::default().sense(), 0x00);
}
