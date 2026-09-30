use machine::{Time, TimeError, TimerPolicy};
use periph::{
    dtim::{BASES as DTIM_BASES, VECTORS as DTIM_VECTORS},
    intc::{BASES as INTC_BASES, VECTOR_BASE},
    pit::{BASES as PIT_BASES, F_BUS, VECTORS},
};

fn enable_pit0(time: &mut Time) {
    time.write(PIT_BASES[0], 2, 0x000b); // EN|RLD|PIE, PRE=0
    time.write(PIT_BASES[0] + 2, 2, 131); // 132 instructions at ips=F_BUS
}

fn unmask_pit0_at_level(time: &mut Time, level: u8) {
    let vector = VECTORS[0];
    let source = u32::from(vector - VECTOR_BASE[2]);
    let base = INTC_BASES[2];
    time.write(base + 0x40 + source, 1, u32::from(level));
    let imrl = base + 0x0c;
    let mask = time.read(imrl, 4).unwrap();
    time.write(imrl, 4, mask & !(1 << source));
}

#[test]
fn blocked_pit_tick_is_retained_then_delivered_once() {
    let mut time = Time::new(TimerPolicy::Oracle, vec![0], F_BUS);
    enable_pit0(&mut time);
    unmask_pit0_at_level(&mut time, 1);
    assert_eq!(time.deadline(0), Some(132));

    time.seed_sr(0x2100); // supervisor, IPL 1: block the level-1 PIT source
    assert_eq!(time.service(132).unwrap(), []);

    time.seed_sr(0x2000); // lower IPL without advancing the guest clock
    assert_eq!(time.service(132).unwrap(), vec![(VECTORS[0], 1)]);
    assert_eq!(time.service(132).unwrap(), []);
}

#[test]
fn fresh_oracle_intc_delivers_pit_after_firmware_icr_setup_without_imr_write() {
    let mut time = Time::new(TimerPolicy::Oracle, vec![0], F_BUS);
    enable_pit0(&mut time);
    let vector = VECTORS[0];
    let source = u32::from(vector - VECTOR_BASE[2]);
    let base = INTC_BASES[2];
    time.write(base + 0x40 + source, 1, 1); // enable source at level 1
    assert_eq!(time.deadline(0), Some(132));

    assert_eq!(time.service(132).unwrap(), vec![(vector, 1)]);
    assert_eq!(time.service(132).unwrap(), []);
}

#[test]
fn time_constructor_keeps_device_intc_hardware_reset_masks() {
    let oracle = Time::new(TimerPolicy::Oracle, vec![], F_BUS);
    let device = Time::new(TimerPolicy::Device, vec![], F_BUS);
    for base in INTC_BASES {
        assert_eq!(oracle.read(base + 0x08, 4), Some(0));
        assert_eq!(oracle.read(base + 0x0c, 4), Some(0));
        assert_eq!(device.read(base + 0x08, 4), Some(u32::MAX));
        assert_eq!(device.read(base + 0x0c, 4), Some(u32::MAX));
    }
}

#[test]
fn pif_write_clears_pending_tick_before_delivery() {
    let mut time = Time::new(TimerPolicy::Oracle, vec![0], F_BUS);
    enable_pit0(&mut time);
    unmask_pit0_at_level(&mut time, 1);
    assert_eq!(time.deadline(0), Some(132));

    time.seed_sr(0x2100); // retain the due tick pending at IPL 1
    assert_eq!(time.service(132).unwrap(), []);
    time.write(PIT_BASES[0] + 1, 1, 0x04); // PCSR PIF write-one-to-clear

    time.seed_sr(0x2000);
    assert_eq!(time.service(132).unwrap(), []);
}

#[test]
fn facade_owns_loads_pages_and_returns_pit_deadline() {
    let mut time = Time::new(TimerPolicy::Oracle, vec![0], F_BUS);
    assert!(Time::owns(PIT_BASES[0]));
    assert!(Time::owns(INTC_BASES[2]));
    assert!(!Time::owns(0));

    let mut pit_page = vec![0; 4];
    pit_page[1] = 0x0b; // PCSR: EN|RLD|PIE
    pit_page[3] = 131; // PMR
    assert!(time.load_page(PIT_BASES[0], &pit_page));
    assert_eq!(time.read(PIT_BASES[0], 2), Some(0x000b));
    assert_eq!(time.deadline(0), Some(132));

    assert!(time.load_page(INTC_BASES[2], &[0x5a]));
    assert_eq!(time.read(INTC_BASES[2], 1), Some(0x5a));
}

#[test]
fn dtim_oracle_offer_preserves_host_write_and_declined_tick() {
    let mut time = Time::with_dtims(TimerPolicy::Oracle, vec![], vec![3], F_BUS);
    assert!(Time::owns(DTIM_BASES[3]));
    time.write(DTIM_BASES[3], 2, 0x001d);
    time.write(DTIM_BASES[3] + 4, 4, 100);
    let vector = DTIM_VECTORS[3];
    let source = u32::from(vector - VECTOR_BASE[0]);
    let base = INTC_BASES[0];
    time.write(base + 0x40 + source, 1, 2);
    let mask = time.read(base + 0x08, 4).unwrap();
    time.write(base + 0x08, 4, mask & !(1 << (source % 32)));
    let due = time.deadline(0).unwrap();
    assert!(time.service_with(due, |_, _| false).unwrap().is_empty());
    assert_eq!(time.take_host_writes().len(), 1);
    assert_eq!(time.service_with(due, |_, _| true).unwrap(), [(vector, 2)]);
    assert!(time.take_host_writes().is_empty());
}

#[test]
fn cross_slot_pit_and_intc_access_is_rejected_without_panicking() {
    let mut time = Time::new(TimerPolicy::Oracle, vec![0], F_BUS);
    assert_eq!(time.read(PIT_BASES[0] + 0x3fff, 2), None);
    assert!(!time.write(PIT_BASES[0] + 0x3fff, 2, 0));
    assert_eq!(time.read(INTC_BASES[2] + 0x3fff, 4), None);
    assert!(!time.write(INTC_BASES[2] + 0x3fff, 4, 0));
}

#[test]
fn device_service_returns_unsupported_without_delivery() {
    let mut time = Time::new(TimerPolicy::Device, vec![0], F_BUS);
    enable_pit0(&mut time);
    unmask_pit0_at_level(&mut time, 1);
    assert_eq!(time.deadline(0), Some(132));

    assert_eq!(
        time.service(132),
        Err(TimeError::DeviceInterruptDeliveryUnsupported)
    );
    assert_eq!(
        time.service(132),
        Err(TimeError::DeviceInterruptDeliveryUnsupported)
    );
}
