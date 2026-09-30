//! Direct Rust invocation is UNVERIFIED. Use `checkpointchain.py first-mmio`
//! to recheck source receipts and build private local inputs before running.

use std::{env, fs, path::Path};

use coldfire::Cpu;
use emmc_card::Card;
use machine::{
    Board, CompletionPolicy, Machine, SemaphoreAddresses, Time, TimerPolicy,
    board::{GuestAccess, GuestAccessKind, GuestBusAccess},
};
use serde::Deserialize;

const MAX_STEPS: usize = 1_000_000;
const MAX_EFFECTS: usize = 1_000_000;
const MAX_TRACE_BYTES: u64 = 96 * 1024 * 1024;

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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GuestEffect {
    kind: String,
    step: usize,
    pc: u32,
    address: u32,
    size: u8,
    value: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CpuRamTrace {
    format_version: u32,
    product: String,
    limit: usize,
    every: usize,
    samples: Vec<CpuSample>,
    effects: Vec<GuestEffect>,
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

fn assert_cpu(cpu: &Cpu, expected: &CpuRegs, product: &str, step: usize) {
    let field = (0..8)
        .find(|&index| cpu.d[index] != expected.d[index])
        .map(|index| format!("D{index}"))
        .or_else(|| {
            (0..8)
                .find(|&index| cpu.a[index] != expected.a[index])
                .map(|index| format!("A{index}"))
        })
        .or_else(|| (cpu.pc != expected.pc).then(|| "PC".to_owned()))
        .or_else(|| (u32::from(cpu.sr) != expected.sr).then(|| "SR".to_owned()));
    assert!(
        field.is_none(),
        "{product}: first sampled CPU-state mismatch at step {step}, field {}; native previous exception {:?}, PC equal {}, SR equal {}",
        field.unwrap_or_default(),
        cpu.last_exception,
        cpu.pc == expected.pc,
        u32::from(cpu.sr) == expected.sr
    );
}

fn assert_effect(
    expected: &GuestEffect,
    kind: &str,
    actual: &GuestAccess,
    product: &str,
    step: usize,
    index: usize,
    pc: u32,
) {
    assert!(
        expected.kind == kind
            && expected.step == step
            && expected.pc == pc
            && expected.address == actual.address
            && expected.size == actual.size
            && expected.value == actual.value,
        "{product}: first ordered guest effect mismatch at step {step}, index {index}"
    );
}

fn check_one(product: &str, state_env: &str, events_env: &str, cpu_env: &str, ram_env: &str) {
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
    let cpu_ram: Option<CpuRamTrace> = env::var(ram_env).ok().map(|_| {
        let path = env::var(ram_env).expect("CPU/RAM trace path");
        assert!(
            fs::metadata(&path).expect("CPU/RAM trace metadata").len() <= MAX_TRACE_BYTES,
            "{product}: CPU/RAM trace exceeds bounded file size"
        );
        serde_json::from_slice(&local_file(ram_env)).expect("CPU/RAM trace must parse")
    });
    assert!(
        cpu_samples.is_none() || cpu_ram.is_none(),
        "choose one CPU source"
    );
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
    if let Some(reference) = &cpu_ram {
        assert_eq!(reference.format_version, 1);
        assert_eq!(reference.product, product);
        assert_eq!(reference.limit, last_step + 1);
        assert!(reference.every > 0 && reference.every <= reference.limit);
        assert_eq!(
            reference.samples.len(),
            (reference.limit - 1) / reference.every + 2,
            "{product}: missing CPU samples"
        );
        for (index, sample) in reference.samples[..reference.samples.len() - 1]
            .iter()
            .enumerate()
        {
            assert_eq!(sample.step, index * reference.every);
        }
        assert!(
            reference
                .samples
                .first()
                .is_some_and(|sample| sample.step == 0)
        );
        assert!(
            reference
                .samples
                .last()
                .is_some_and(|sample| sample.step == reference.limit)
        );
        assert!(
            reference
                .samples
                .windows(2)
                .all(|pair| pair[0].step < pair[1].step)
        );
        assert!(reference.effects.len() <= MAX_EFFECTS);
        assert!(
            reference
                .effects
                .windows(2)
                .all(|pair| pair[0].step <= pair[1].step)
        );
        for effect in &reference.effects {
            assert!(effect.step < reference.limit);
            assert!(matches!(effect.size, 1 | 2 | 4));
            match effect.kind.as_str() {
                "RAM_WR" => {
                    assert!(u64::from(effect.address) + u64::from(effect.size) <= 0x8000_0000);
                }
                "RD" | "WR" => assert!(recorded_mmio(effect.address)),
                _ => panic!("{product}: unknown CPU/RAM effect kind"),
            }
        }
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
    let mut effect_index = 0;
    let mut ram_writes = 0;
    // Recorder._clock() labels the instruction currently executing with
    // _step_base + (_ic - 1); the first instruction is offset zero.
    for step in 0..=last_step {
        let pc = machine.cpu.pc;
        let samples = cpu_ram
            .as_ref()
            .map(|reference| reference.samples.as_slice())
            .or_else(|| cpu_samples.as_ref().map(|source| source.samples.as_slice()));
        if let Some(sample) = samples.and_then(|samples| samples.get(sample_index))
            && sample.step == step
        {
            // Cpu::step intentionally leaves NZV lazy; its public SR must be
            // resolved before comparing against Unicorn's reg_read(SR).
            machine.cpu.resolve_nzv();
            assert_cpu(&machine.cpu, &sample.regs, product, step);
            sample_index += 1;
        }
        machine.board.clear_guest_accesses();
        machine.step_timed().unwrap_or_else(|error| {
            panic!("{product}: first native stop at step {step} before MMIO parity: {error:?}")
        });
        for GuestBusAccess { kind, access } in machine.board.take_guest_accesses() {
            let effect_kind = if recorded_mmio(access.address) {
                match kind {
                    GuestAccessKind::Read => "RD",
                    GuestAccessKind::Write => "WR",
                }
            } else if kind == GuestAccessKind::Write
                && u64::from(access.address) + u64::from(access.size) <= 0x8000_0000
            {
                "RAM_WR"
            } else {
                continue;
            };
            if let Some(reference) = &cpu_ram {
                let Some(expected) = reference.effects.get(effect_index) else {
                    panic!("{product}: extra {effect_kind} at step {step}, index {effect_index}");
                };
                assert_effect(
                    expected,
                    effect_kind,
                    &access,
                    product,
                    step,
                    effect_index,
                    pc,
                );
                effect_index += 1;
                if effect_kind == "RAM_WR" {
                    ram_writes += 1;
                }
            }
            if effect_kind == "RAM_WR" {
                continue;
            }
            let Some(expected) = window.events.get(seen) else {
                panic!(
                    "{product}: extra native {effect_kind} at step {step} after recorded window"
                );
            };
            assert!(
                expected.kind == effect_kind
                    && expected.step == step
                    && expected.address == access.address
                    && expected.value == access.value
                    && expected.pc == pc
                    && expected.size == access.size,
                "{product}: first MMIO mismatch at native step {step}, event {seen}; kind/clock/address/value/PC/size differ"
            );
            seen += 1;
        }
        if let Some(reference) = &cpu_ram
            && let Some(next) = reference.effects.get(effect_index)
        {
            assert!(
                next.step > step,
                "{product}: missing ordered guest effect at step {step}"
            );
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
    if let Some(reference) = &cpu_ram {
        assert_eq!(
            effect_index,
            reference.effects.len(),
            "{product}: ordered effect coverage"
        );
        let last = reference.samples.last().expect("final CPU sample");
        machine.cpu.resolve_nzv();
        assert_cpu(&machine.cpu, &last.regs, product, last_step + 1);
        assert_eq!(sample_index + 1, reference.samples.len());
        println!(
            "{product}: compared {} sampled CPU boundaries, {} ordered guest RAM writes, and {seen} ordered MMIO accesses through step {last_step}",
            reference.samples.len(),
            ram_writes
        );
    }
    assert_eq!(
        seen,
        window.events.len(),
        "{product}: insufficient MMIO coverage"
    );
    machine.board.set_guest_access_capture(false);
}

#[test]
fn corrupt_guest_ram_write_fails_at_the_first_effect() {
    let expected = GuestEffect {
        kind: "RAM_WR".to_owned(),
        step: 3,
        pc: 0x4000_1000,
        address: 0x4000_2000,
        size: 4,
        value: 0x1234,
    };
    let wrong = GuestAccess {
        address: expected.address,
        size: expected.size,
        value: 0x1235,
    };
    assert!(
        std::panic::catch_unwind(|| {
            assert_effect(&expected, "RAM_WR", &wrong, "test", 3, 0, expected.pc);
        })
        .is_err()
    );
}

#[test]
fn mixed_ram_and_mmio_effect_order_is_not_interchangeable() {
    let expected = GuestEffect {
        kind: "RAM_WR".to_owned(),
        step: 3,
        pc: 0x4000_1000,
        address: 0x4000_2000,
        size: 4,
        value: 1,
    };
    let native_read = GuestAccess {
        address: 0xFC00_0000,
        size: 4,
        value: 1,
    };
    assert!(
        std::panic::catch_unwind(|| {
            assert_effect(&expected, "RD", &native_read, "test", 3, 0, expected.pc);
        })
        .is_err()
    );
}

#[test]
fn corrupt_cpu_boundary_is_detected() {
    let cpu = Cpu::new();
    let mut expected = CpuRegs {
        d: cpu.d,
        a: cpu.a,
        pc: cpu.pc,
        sr: u32::from(cpu.sr),
    };
    expected.d[0] ^= 1;
    assert!(std::panic::catch_unwind(|| assert_cpu(&cpu, &expected, "test", 2)).is_err());
}

#[test]
#[ignore = "requires ignored source-checked local artifacts; first divergence may still fail"]
fn first_guest_mmio_mismatch_dt2() {
    check_one(
        "dt2",
        "DT2_LOCAL_MSTATE",
        "DT2_LOCAL_EVENTS",
        "DT2_CPU_SPARSE",
        "DT2_LOCAL_CPU_RAM",
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
        "DN2_LOCAL_CPU_RAM",
    );
}
