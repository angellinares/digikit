use coldfire::Cpu;
use emmc_card::Card;
use machine::MachineState;
use machine::timer_state::{TimerStateError, import_timers};
use machine::{Board, CompletionPolicy, Machine, SemaphoreAddresses, StateApplyError};
use machine::{Time, TimerPolicy};
use periph::machine::Timers;
use serde_json::json;

fn state(timers: serde_json::Value) -> MachineState {
    MachineState {
        clock: 0,
        regs: machine::state::Registers {
            d: [0; 8],
            a: [0; 8],
            pc: 0,
            sr: 0,
        },
        ctlregs: Default::default(),
        mapped_bases: vec![],
        mmio_forced: Default::default(),
        ff1_count: 0,
        movec_count: 0,
        components: json!({"timers": timers}),
        manifest: serde_json::Value::Null,
        pages: vec![],
    }
}

fn component() -> serde_json::Value {
    json!({
        "type": "Timers", "version": 1,
        "sources": [
            {"type": "Pits", "version": 1, "channels": [3, 2, 0], "ips": 4680000,
             "next": [null, null, 125.5, 101.0], "now": 100, "held": true,
             "fired": {"3": 4}, "missed": {"2": 5}, "cleared": {"0": 6}, "pending": [2]},
            {"type": "Dtims", "version": 1, "channels": [3], "ips": 4680000,
             "next": [null, null, null, 130.0], "now": 100, "held": false,
             "fired": {"3": 7}, "missed": {"3": 8}, "cleared": {"3": 9},
             "pending": [3], "arm": [], "stale": [3]}
        ]
    })
}

#[test]
fn imports_timer_component_with_checkpoint_relative_deadlines() {
    let mut timers = Timers::new(vec![3, 2, 0], vec![3], 4_680_000.0);
    import_timers(&state(component()), &mut timers).unwrap();
    assert_eq!(timers.pit.next_deadline(2), Some(25.5));
    assert_eq!(timers.pit.next_deadline(3), Some(1.0));
    assert!(timers.pit.pending(2));
    assert_eq!(timers.pit.fired(3), 4);
    assert_eq!(timers.pit.missed(2), 5);
    assert_eq!(timers.pit.cleared(0), 6);
    assert_eq!(timers.dtim.next_deadline(3), Some(30.0));
    assert!(timers.dtim.pending(3));
    assert_eq!(timers.dtim.fired(3), 7);
    assert_eq!(timers.dtim.missed(3), 8);
    assert_eq!(timers.dtim.cleared(3), 9);
    assert_eq!(timers.dtim.stale(), &[3]);
}

#[test]
fn time_facade_imports_both_sources_without_discarding_loaded_registers() {
    let mut time = Time::with_dtims(TimerPolicy::Oracle, vec![3, 2, 0], vec![3], 4_680_000.0);
    let dtim3 = periph::dtim::BASES[3];
    time.write(dtim3, 2, 0x001d);
    time.restore_timer_component(&state(component())).unwrap();
    assert_eq!(time.read(dtim3, 2), Some(0x001d));
    let mut wrong_topology = Time::new(TimerPolicy::Oracle, vec![3, 2, 0], 4_680_000.0);
    assert_eq!(
        wrong_topology.restore_timer_component(&state(component())),
        Err(TimerStateError::Configuration)
    );
}

#[test]
fn machine_imports_timer_component_before_running_guest_instructions() {
    let mut unattached = Machine::new(
        Cpu::new(),
        Board::new(
            Card::default(),
            SemaphoreAddresses::default(),
            CompletionPolicy::Oracle,
        ),
    );
    assert_eq!(
        unattached.apply_state(&state(component())),
        Err(StateApplyError::TimerNotAttached)
    );
    let mut machine = Machine::new(
        Cpu::new(),
        Board::new(
            Card::default(),
            SemaphoreAddresses::default(),
            CompletionPolicy::Oracle,
        ),
    );
    let mut time = Time::with_dtims(TimerPolicy::Oracle, vec![3, 2, 0], vec![3], 4_680_000.0);
    let dtim3 = periph::dtim::BASES[3];
    time.write(dtim3, 2, 0x001b);
    time.write(dtim3 + 4, 4, 10);
    machine.board.attach_time(time);
    machine.apply_state(&state(component())).unwrap();
    assert_eq!(machine.board.time_mut().unwrap().deadline(0), Some(30));

    let mut wrong_source = component();
    wrong_source["sources"][1]["channels"] = json!([2]);
    assert_eq!(
        machine.apply_state(&state(wrong_source)),
        Err(StateApplyError::TimerState(TimerStateError::Configuration))
    );
}

#[test]
fn rejects_configuration_and_unrepresentable_dtim_arm() {
    let mut timers = Timers::new(vec![3], vec![3], 4_680_000.0);
    assert_eq!(
        import_timers(&state(component()), &mut timers),
        Err(TimerStateError::Configuration)
    );

    let mut component = component();
    component["sources"][1]["arm"] = json!([1]);
    let mut timers = Timers::new(vec![3, 2, 0], vec![3], 4_680_000.0);
    assert_eq!(
        import_timers(&state(component), &mut timers),
        Err(TimerStateError::Unsupported)
    );
}

#[test]
fn accepts_legacy_missing_pending_and_cleared_for_both_sources() {
    let mut component = component();
    for index in 0..2 {
        let source = component["sources"][index].as_object_mut().unwrap();
        source.remove("pending");
        source.remove("cleared");
    }
    let mut timers = Timers::new(vec![3, 2, 0], vec![3], 4_680_000.0);
    import_timers(&state(component), &mut timers).unwrap();
    assert!(!timers.pit.pending(2));
    assert_eq!(timers.pit.cleared(0), 0);
    assert!(!timers.dtim.pending(3));
    assert_eq!(timers.dtim.cleared(3), 0);
}

#[test]
fn rejects_invalid_u32_timer_scalars_without_partial_application() {
    for (source, field, value) in [
        (0, "ips", json!(4_680_000.5)),
        (1, "now", json!(100.5)),
        (0, "now", json!(u64::from(u32::MAX) + 1)),
        (1, "fired", json!({"3": u64::from(u32::MAX) + 1})),
    ] {
        let mut component = component();
        component["sources"][source][field] = value;
        let mut timers = Timers::new(vec![3, 2, 0], vec![3], 4_680_000.0);
        assert_eq!(
            import_timers(&state(component), &mut timers),
            Err(TimerStateError::Invalid)
        );
        assert_eq!(timers.pit.next_deadline(0), None);
        assert_eq!(timers.dtim.next_deadline(3), None);
    }
}

#[test]
fn preserves_overdue_deadline_for_service_at_checkpoint_zero() {
    let mut component = component();
    component["sources"][0]["held"] = json!(false);
    component["sources"][0]["next"][3] = json!(99.0);
    let mut timers = Timers::new(vec![3, 2, 0], vec![3], 4_680_000.0);
    timers.write(periph::pit::BASES[3], 2, 0x0009);
    timers.write(periph::pit::BASES[3] + 2, 2, 131);
    import_timers(&state(component), &mut timers).unwrap();
    assert_eq!(timers.pit.next_deadline(3), Some(-1.0));
    timers.service(0);
    assert!(timers.pit.pending(3));
}

#[test]
fn accepts_duplicate_and_unconfigured_pending_and_stale_channels() {
    let mut component = component();
    component["sources"][0]["pending"] = json!([1, 1]);
    component["sources"][1]["pending"] = json!([0, 0]);
    component["sources"][1]["stale"] = json!([0, 0, 2]);
    let mut timers = Timers::new(vec![3, 2, 0], vec![3], 4_680_000.0);
    import_timers(&state(component), &mut timers).unwrap();
    assert!(timers.pit.pending(1));
    assert!(timers.dtim.pending(0));
    assert_eq!(timers.dtim.stale(), &[0, 0, 2]);
}

#[test]
fn rejects_bad_dtim_without_mutating_valid_pit() {
    let mut component = component();
    component["sources"][1]["next"][3] = json!("not a deadline");
    let mut timers = Timers::new(vec![3, 2, 0], vec![3], 4_680_000.0);
    assert_eq!(
        import_timers(&state(component), &mut timers),
        Err(TimerStateError::Invalid)
    );
    assert_eq!(timers.pit.next_deadline(2), None);
}
