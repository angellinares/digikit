use coldfire::Cpu;
use emmc_card::{Card, DEFAULT_CAPACITY_BLOCKS, SMALL_CAPACITY_BLOCKS};
use machine::{
    Board, CompletionPolicy, Machine, MachineState, SemaphoreAddresses, StateApplyError, Time,
    TimerPolicy, state::Registers,
};
use periph::esdhc;
use serde_json::json;

fn source() -> MachineState {
    MachineState {
        clock: 0,
        regs: Registers {
            d: [0; 8],
            a: [0; 8],
            pc: 0,
            sr: 0x2000,
        },
        ctlregs: Default::default(),
        mapped_bases: vec![],
        mmio_forced: Default::default(),
        ff1_count: 0,
        movec_count: 0,
        components: json!({
            "timers": {"type": "Timers", "version": 1, "sources": [
                {"type": "Pits", "version": 1, "channels": [3,2,0], "ips": 4680000,
                 "next": [null,null,null,null], "now": 0, "held": false,
                 "fired": {}, "missed": {}, "cleared": {}, "pending": []},
                {"type": "Dtims", "version": 1, "channels": [3], "ips": 4680000,
                 "next": [null,null,null,null], "now": 0, "held": false,
                 "fired": {}, "missed": {}, "cleared": {}, "pending": [],
                 "arm": [], "stale": []}
            ]},
            "esdhc": {"type": "Esdhc", "version": 1,
                "pattern": 0x12345678, "armed": 59, "dma_bytes": 0,
                "card_blocks": DEFAULT_CAPACITY_BLOCKS, "card_rca": 0x1234,
                "card_selected": true, "card_overlay": {"0": 0, "513": 0x5a}},
            "edma_tx": {"type": "TxChannel", "version": 1, "chan": 35,
                "vector": 155, "pending": 0, "bytes": 0, "transfers": 0},
            "uart_in": {"type": "deque", "version": 1, "values": []}
        }),
        manifest: serde_json::Value::Null,
        pages: vec![],
    }
}

fn machine() -> Machine {
    let mut machine = Machine::new(
        Cpu::new(),
        Board::new(
            Card::default(),
            SemaphoreAddresses::default(),
            CompletionPolicy::Oracle,
        ),
    );
    machine.board.attach_time(Time::with_dtims(
        TimerPolicy::Oracle,
        vec![3, 2, 0],
        vec![3],
        4_680_000.0,
    ));
    machine
}

#[test]
fn dormant_python_v1_host_state_restores_storage_and_timers() {
    let mut machine = machine();
    machine.apply_state(&source()).unwrap();
    assert!(machine.board.dma59_armed());
    let card = machine.board.esdhc.card_mut();
    assert_eq!(card.rca(), 0x1234);
    assert!(card.selected());
    assert_eq!(card.overlay_len(), 2);
    assert_eq!(card.data_for(18, 0, 2).unwrap().unwrap(), [0, 0]);
    machine
        .board
        .esdhc
        .write(esdhc::BASE + esdhc::XFERTYP, 4, 0x0e3a_0010);
    assert_eq!(
        machine.board.esdhc.read(esdhc::BASE + esdhc::DATPORT, 4),
        Some(!0x1234_5678)
    );
}

#[test]
fn active_uart_or_pending_tx_state_is_rejected_before_application() {
    let mut input = source();
    input.components["uart_in"]["values"] = json!([0x41]);
    assert_eq!(
        machine().apply_state(&input),
        Err(StateApplyError::UnsupportedComponents)
    );
    input.components["uart_in"]["values"] = json!([]);
    input.components["edma_tx"]["pending"] = json!(1);
    assert_eq!(
        machine().apply_state(&input),
        Err(StateApplyError::UnsupportedComponents)
    );
    input.components["edma_tx"]["pending"] = json!(0);
}

#[test]
fn nonzero_diagnostic_counters_restore_without_pending_work() {
    let mut input = source();
    input.components["esdhc"]["dma_bytes"] = json!(42_312_704_u64);
    input.components["edma_tx"]["bytes"] = json!(47_114_u64);
    input.components["edma_tx"]["transfers"] = json!(14_220_u64);
    let mut native = machine();
    native.apply_state(&input).unwrap();
    assert_eq!(native.board.esdhc_dma_bytes(), 42_312_704);
    assert_eq!(native.board.dma.tx35.bytes, 47_114);
    assert_eq!(native.board.dma.tx35.transfers, 14_220);
    assert_eq!(native.board.dma.tx35.pending, 0);

    input.components["esdhc"]["dma_bytes"] = json!(-1);
    assert_eq!(
        machine().apply_state(&input),
        Err(StateApplyError::InvalidComponents)
    );
}

#[test]
fn python_host_state_requires_oracle_board_and_timer_policies() {
    let mut device = Machine::new(
        Cpu::new(),
        Board::new(
            Card::default(),
            SemaphoreAddresses::default(),
            CompletionPolicy::Device,
        ),
    );
    device.board.attach_time(Time::with_dtims(
        TimerPolicy::Oracle,
        vec![3, 2, 0],
        vec![3],
        4_680_000.0,
    ));
    assert_eq!(
        device.apply_state(&source()),
        Err(StateApplyError::OracleHostStateRequired)
    );
    let mut device_time = machine();
    device_time.board.attach_time(Time::with_dtims(
        TimerPolicy::Device,
        vec![3, 2, 0],
        vec![3],
        4_680_000.0,
    ));
    assert_eq!(
        device_time.apply_state(&source()),
        Err(StateApplyError::OracleHostStateRequired)
    );
}

#[test]
fn invalid_or_different_card_state_cannot_be_accepted_as_matching() {
    let mut input = source();
    input.components["esdhc"]["card_overlay"] = json!({"00": 0});
    assert_eq!(
        machine().apply_state(&input),
        Err(StateApplyError::InvalidComponents)
    );
    input.components["esdhc"]["card_overlay"] = json!({});
    input.components["esdhc"]["card_blocks"] = json!(SMALL_CAPACITY_BLOCKS);
    assert_eq!(
        machine().apply_state(&input),
        Err(StateApplyError::CardCapacityMismatch {
            expected: DEFAULT_CAPACITY_BLOCKS,
            actual: SMALL_CAPACITY_BLOCKS,
        })
    );
    input.components["esdhc"]["card_blocks"] = json!(DEFAULT_CAPACITY_BLOCKS);
    input.components["esdhc"]["armed"] = json!(64);
    assert_eq!(
        machine().apply_state(&input),
        Err(StateApplyError::UnsupportedComponents)
    );
    input.components["esdhc"]["armed"] = json!(null);
    input.components["esdhc"]["pattern"] = json!(true);
    assert_eq!(
        machine().apply_state(&input),
        Err(StateApplyError::InvalidComponents)
    );
    input.components["esdhc"]["pattern"] = json!(0);
    input.components["uart_in"]["values"] = json!([256]);
    assert_eq!(
        machine().apply_state(&input),
        Err(StateApplyError::InvalidComponents)
    );
}

#[test]
fn legacy_channel_35_armed_record_does_not_arm_storage_dma59() {
    let mut input = source();
    input.components["esdhc"]["armed"] = json!(35);
    let mut machine = machine();
    machine.apply_state(&input).unwrap();
    assert!(!machine.board.dma59_armed());
}

#[test]
#[ignore = "requires local, operator-verified ignored MSTATE; direct Rust input has no provenance"]
fn locally_verified_dt2_and_dn2_pre_mmio_states_import() {
    for name in ["DT2_LOCAL_MSTATE", "DN2_LOCAL_MSTATE"] {
        let path = std::env::var(name).expect("local ignored MSTATE path required");
        let bytes = std::fs::read(path).expect("local MSTATE must be readable");
        let state = machine::state::parse(&bytes).expect("portable state must parse");
        let mut machine = Machine::new(
            Cpu::new(),
            Board::new(
                Card::default(),
                SemaphoreAddresses::default(),
                CompletionPolicy::Oracle,
            ),
        );
        machine.board.attach_time(Time::with_dtims(
            TimerPolicy::Oracle,
            vec![3, 2, 0],
            vec![3],
            132_000_000.0,
        ));
        machine
            .apply_state(&state)
            .unwrap_or_else(|error| panic!("{name}: {error:?}"));
        assert_eq!(machine.clock, state.clock);
        assert_eq!(machine.cpu.pc, state.regs.pc);
    }
}

#[test]
#[ignore = "requires locally checked auto-ready MSTATE; direct Rust input does not authenticate it"]
fn locally_checked_auto_ready_counters_import() {
    let path = std::env::var("DT2_AUTO_READY_MSTATE").expect("local MSTATE path required");
    let bytes = std::fs::read(path).expect("local MSTATE must be readable");
    let state = machine::state::parse(&bytes).expect("portable state must parse");
    let mut native = machine();
    // The timer component in this fixture uses the device's 132M guest IPS.
    native.board.attach_time(Time::with_dtims(
        TimerPolicy::Oracle,
        vec![3, 2, 0],
        vec![3],
        132_000_000.0,
    ));
    native.apply_state(&state).unwrap();
    assert_eq!(native.clock, state.clock);
    assert_eq!(native.cpu.pc, state.regs.pc);
    for (value, actual) in [
        (
            &state.components["esdhc"]["dma_bytes"],
            native.board.esdhc_dma_bytes(),
        ),
        (
            &state.components["edma_tx"]["bytes"],
            native.board.dma.tx35.bytes,
        ),
        (
            &state.components["edma_tx"]["transfers"],
            native.board.dma.tx35.transfers,
        ),
    ] {
        assert!(actual > 0);
        assert_eq!(value.as_u64(), Some(actual));
    }
    assert_eq!(native.board.dma.tx35.pending, 0);
}
