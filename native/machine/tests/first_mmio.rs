//! Direct Rust invocation is UNVERIFIED. Use `checkpointchain.py first-mmio`
//! to recheck source receipts and build private local inputs before running.

use std::{env, fs, path::Path};

use coldfire::Cpu;
use emmc_card::Card;
use machine::{
    Board, CompletionPolicy, Machine, SemaphoreAddresses, Time, TimerPolicy,
    board::{GuestAccessKind, GuestBusAccess},
};
use serde::Deserialize;

const MAX_STEPS: usize = 1_000_000;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Expected {
    kind: String,
    step: usize,
    address: u32,
    value: u32,
    pc: u32,
    size: u8,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Coverage {
    #[serde(rename = "RD")]
    reads: usize,
    #[serde(rename = "WR")]
    writes: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Window {
    format_version: u32,
    count: usize,
    window_done: usize,
    coverage: Coverage,
    events: Vec<Expected>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CpuRegs {
    d: [u32; 8],
    a: [u32; 8],
    pc: u32,
    sr: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CpuSample {
    step: usize,
    regs: CpuRegs,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CpuSamples {
    product: String,
    limit: usize,
    every: usize,
    samples: Vec<CpuSample>,
}

fn local_file(env_name: &str) -> Vec<u8> {
    let path = env::var(env_name).unwrap_or_else(|_| panic!("missing {env_name}"));
    assert!(
        Path::new(&path).is_absolute(),
        "{env_name} must be absolute"
    );
    fs::read(path).unwrap_or_else(|_| panic!("{env_name} must be readable"))
}

fn recorded_mmio(addr: u32) -> bool {
    (0x8c00_0000..=0x8fff_ffff).contains(&addr) || addr >= 0xc000_0000
}

fn check_one(product: &str, state_env: &str, events_env: &str, cpu_env: &str) {
    let state = machine::state::parse(&local_file(state_env)).expect("local MSTATE must parse");
    let window: Window =
        serde_json::from_slice(&local_file(events_env)).expect("local MMIO event JSON must parse");
    assert_eq!(window.format_version, 1);
    assert!(window.coverage.reads > 0 && window.coverage.writes > 0);
    assert!((2..=64).contains(&window.count));
    assert_eq!(window.count, window.events.len());
    assert!(window.events.iter().any(|event| event.kind == "RD"));
    assert!(window.events.iter().any(|event| event.kind == "WR"));
    let last_step = window.events.last().unwrap().step;
    assert!(last_step > 0 && last_step < window.window_done && last_step <= MAX_STEPS);
    assert!(
        window
            .events
            .windows(2)
            .all(|pair| pair[0].step <= pair[1].step)
    );
    let cpu_samples: Option<CpuSamples> = env::var(cpu_env).ok().map(|_| {
        serde_json::from_slice(&local_file(cpu_env)).expect("local CPU sample JSON must parse")
    });
    if let Some(samples) = &cpu_samples {
        assert_eq!(samples.product, product);
        assert!(samples.limit <= MAX_STEPS && samples.every > 0);
        assert!(samples.samples.len() <= 2_000);
        assert!(
            samples
                .samples
                .first()
                .is_some_and(|sample| sample.step == 0)
        );
        assert!(
            samples
                .samples
                .windows(2)
                .all(|pair| pair[0].step < pair[1].step)
        );
    }

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
        .expect("local host state must import");
    // Python Machine._fault first-touch maps zero pages; the imported
    // checkpoint need not contain SDRAM pages first touched in this window.
    machine.board.enable_oracle_sdram_faults();
    machine.board.set_guest_access_capture(true);
    let mut seen = 0;
    let mut sample_index = 0;
    // Recorder._clock() labels the instruction currently executing with
    // _step_base + (_ic - 1); the first instruction is offset zero.
    for step in 0..=last_step {
        let pc = machine.cpu.pc;
        if let Some(samples) = &cpu_samples
            && let Some(sample) = samples.samples.get(sample_index)
            && sample.step == step
        {
            let actual = &machine.cpu;
            let expected = &sample.regs;
            let field = (0..8)
                .find(|&index| actual.d[index] != expected.d[index])
                .map(|index| format!("D{index}"))
                .or_else(|| {
                    (0..8)
                        .find(|&index| actual.a[index] != expected.a[index])
                        .map(|index| format!("A{index}"))
                })
                .or_else(|| (actual.pc != expected.pc).then(|| "PC".to_owned()))
                .or_else(|| (u32::from(actual.sr) != expected.sr).then(|| "SR".to_owned()));
            assert!(
                field.is_none(),
                "{product}: first sampled CPU-state mismatch at step {step}, field {}; native previous exception {:?}, PC equal {}, SR equal {}",
                field.unwrap_or_default(),
                actual.last_exception,
                actual.pc == expected.pc,
                u32::from(actual.sr) == expected.sr
            );
            sample_index += 1;
        }
        machine.board.clear_guest_accesses();
        machine.step_timed().unwrap_or_else(|error| {
            panic!("{product}: first native stop at step {step} before MMIO parity: {error:?}")
        });
        for GuestBusAccess { kind, access } in machine.board.take_guest_accesses() {
            if !recorded_mmio(access.address) {
                continue;
            }
            let kind = match kind {
                GuestAccessKind::Read => "RD",
                GuestAccessKind::Write => "WR",
            };
            let Some(expected) = window.events.get(seen) else {
                panic!("{product}: extra native {kind} at step {step} after recorded window");
            };
            assert!(
                expected.kind == kind
                    && expected.step == step
                    && expected.address == access.address
                    && expected.value == access.value
                    && expected.pc == pc
                    && expected.size == access.size,
                "{product}: first MMIO mismatch at native step {step}, event {seen}; kind/clock/address/value/PC/size differ"
            );
            seen += 1;
        }
        if let Some(expected) = window.events.get(seen) {
            if expected.step == step && expected.pc != pc {
                panic!("{product}: CPU PC already differs at expected MMIO step {step}");
            }
            assert!(
                expected.step > step,
                "{product}: expected MMIO event {seen} missing at step {step}"
            );
        }
    }
    assert_eq!(
        seen,
        window.events.len(),
        "{product}: insufficient MMIO coverage"
    );
    machine.board.set_guest_access_capture(false);
}

#[test]
#[ignore = "requires ignored source-checked local artifacts; first divergence may still fail"]
fn first_guest_mmio_mismatch_dt2() {
    check_one(
        "dt2",
        "DT2_LOCAL_MSTATE",
        "DT2_LOCAL_EVENTS",
        "DT2_CPU_SPARSE",
    );
}

#[test]
#[ignore = "requires ignored source-checked local artifacts; first divergence may still fail"]
fn first_guest_mmio_mismatch_dn2() {
    check_one(
        "dn2",
        "DN2_LOCAL_MSTATE",
        "DN2_LOCAL_EVENTS",
        "DN2_CPU_SPARSE",
    );
}
