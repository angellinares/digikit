//! Opt-in native comparison against bounded local Python-oracle traces.

use std::{env, path::Path};

use coldfire::{Bus, Cpu, Stop};
use emmc_card::Card;
use machine::{
    Board, CompletionEvent, CompletionPolicy, Machine, SemaphoreAddresses, state::parse,
};
use serde::Deserialize;

const ACK: &str = "unverified-local-inputs";
const MAX_STEPS: usize = 1000;
const MAX_TRACE_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Debug, Deserialize)]
struct Trace {
    format_version: u32,
    product: String,
    limit: usize,
    states: Vec<Registers>,
}

#[derive(Debug, Deserialize)]
struct Registers {
    d: [u32; 8],
    a: [u32; 8],
    pc: u32,
    sr: u16,
    clock: u64,
}

fn machine(path_env: &str, product: &str) -> Machine {
    let path = env::var(path_env).unwrap_or_else(|_| panic!("{product}: missing {path_env}"));
    let path = Path::new(&path);
    assert!(path.is_absolute(), "{product}: {path_env} must be absolute");
    let state = parse(&std::fs::read(path).expect("checkpoint fixture is unreadable"))
        .expect("checkpoint MSTATE is invalid");
    let mut machine = Machine::new(
        Cpu::new(),
        Board::new(
            Card::default(),
            SemaphoreAddresses::default(),
            CompletionPolicy::Oracle,
        ),
    );
    machine
        .apply_state(&state)
        .expect("checkpoint state is unsupported");
    machine
}

fn diff_limit(product: &str) -> usize {
    let limit = env::var("NATIVE_CHECKPOINT_DIFF_LIMIT")
        .unwrap_or_else(|_| panic!("{product}: direct Rust differential is UNVERIFIED; NATIVE_CHECKPOINT_DIFF_LIMIT is required"))
        .parse::<usize>()
        .unwrap_or_else(|_| panic!("{product}: NATIVE_CHECKPOINT_DIFF_LIMIT is invalid"));
    assert!(
        (1..=MAX_STEPS).contains(&limit),
        "{product}: NATIVE_CHECKPOINT_DIFF_LIMIT must be 1..={MAX_STEPS}"
    );
    limit
}

fn trace(path_env: &str, product: &str, limit: usize) -> Trace {
    let path = env::var(path_env).unwrap_or_else(|_| panic!("{product}: missing {path_env}"));
    let path = Path::new(&path);
    assert!(
        path.is_absolute() && path.is_file(),
        "{product}: trace is missing"
    );
    let metadata = std::fs::metadata(path).expect("trace metadata is unreadable");
    assert!(
        metadata.len() <= MAX_TRACE_BYTES,
        "{product}: trace is too large"
    );
    let trace: Trace = serde_json::from_slice(&std::fs::read(path).expect("trace is unreadable"))
        .unwrap_or_else(|_| panic!("{product}: trace is invalid"));
    assert_eq!(trace.format_version, 1, "{product}: trace version");
    assert_eq!(trace.product, product, "{product}: trace product");
    assert_eq!(trace.limit, limit, "{product}: trace limit");
    assert_eq!(
        trace.states.len(),
        limit + 1,
        "{product}: trace is truncated"
    );
    trace
}

fn mismatch(product: &str, step: usize, field: impl std::fmt::Display) -> ! {
    panic!("{product}: step={step} field={field}")
}

fn compare(machine: &Machine, expected: &Registers, product: &str, step: usize) {
    for index in 0..8 {
        if machine.cpu.d[index] != expected.d[index] {
            mismatch(product, step, format!("D{index}"));
        }
        if machine.cpu.a[index] != expected.a[index] {
            mismatch(product, step, format!("A{index}"));
        }
    }
    if machine.cpu.pc != expected.pc {
        mismatch(product, step, "PC");
    }
    if machine.cpu.sr != expected.sr {
        mismatch(product, step, "SR");
    }
    if machine.clock != expected.clock {
        mismatch(product, step, "clock");
    }
}

fn movec_issue_at(machine: &mut Machine) -> Result<Option<&'static str>, ()> {
    let opcode = machine.board.read16(machine.cpu.pc).map_err(|_| ())?;
    if opcode == 0x4e7a {
        return Ok(Some("unsupported_movec_read"));
    }
    if opcode != 0x4e7b {
        return Ok(None);
    }
    let selector = machine
        .board
        .read16(machine.cpu.pc.wrapping_add(2))
        .map_err(|_| ())?
        & 0x0fff;
    let known = matches!(selector, 0x002 | 0x003 | 0x004..=0x009 | 0x00c..=0x00f | 0x800 | 0x801 | 0x80e | 0x80f | 0xc04 | 0xc05);
    Ok((!known).then_some("unsupported_movec_unknown_write"))
}

fn run(product: &str, mstate_env: &str, trace_env: &str) {
    assert_eq!(
        env::var("NATIVE_CHECKPOINT_UNVERIFIED_SMOKE_ACK").as_deref(),
        Ok(ACK),
        "{product}: direct Rust differential is UNVERIFIED; set NATIVE_CHECKPOINT_UNVERIFIED_SMOKE_ACK={ACK}"
    );
    let limit = diff_limit(product);
    let mut machine = machine(mstate_env, product);
    let oracle = trace(trace_env, product, limit);
    compare(&machine, &oracle.states[0], product, 0);
    for step in 0..limit {
        if let Some(category) =
            movec_issue_at(&mut machine).expect("bus failure before instruction")
        {
            panic!("{product}: step={step} stop={category}");
        }
        let events = match machine.step() {
            Ok(events) => events,
            Err(Stop::Halted) => panic!("{product}: step={step} stop=halted"),
            Err(Stop::Unimplemented(_)) => panic!("{product}: step={step} stop=unimplemented"),
        };
        if let Some(event) = events.first() {
            let category = match event {
                CompletionEvent::Dma59 { .. } => "completion_dma59",
                CompletionEvent::Data { .. } => "completion_data",
                CompletionEvent::Command { .. } => "completion_command",
            };
            panic!("{product}: step={step} stop={category}");
        }
        compare(&machine, &oracle.states[step + 1], product, step + 1);
    }
}

#[test]
fn corrupted_oracle_register_at_step_one_is_detected() {
    let machine = Machine::new(
        Cpu::new(),
        Board::new(
            Card::default(),
            SemaphoreAddresses::default(),
            CompletionPolicy::Oracle,
        ),
    );
    let bad = Registers {
        d: [1; 8],
        a: [0; 8],
        pc: 0,
        sr: 0,
        clock: 0,
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        compare(&machine, &bad, "test", 1)
    }));
    assert!(result.is_err());
}

#[test]
#[ignore = "requires provenance-generated local oracle traces"]
fn dt2_checkpoint_diff() {
    run("dt2", "DT2_CHECKPOINT_MSTATE", "DT2_CHECKPOINT_TRACE");
}

#[test]
#[ignore = "requires provenance-generated local oracle traces"]
fn dn2_checkpoint_diff() {
    run("dn2", "DN2_CHECKPOINT_MSTATE", "DN2_CHECKPOINT_TRACE");
}
