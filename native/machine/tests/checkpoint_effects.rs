//! Opt-in provenance-gated per-instruction guest Bus effect comparison.

use std::{env, path::Path};

use coldfire::{Bus, Cpu, Stop};
use emmc_card::Card;
use machine::{
    Board, CompletionEvent, CompletionPolicy, Machine, SemaphoreAddresses, board::GuestAccess,
    state::parse,
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
    steps: Vec<Effects>,
}

#[derive(Debug, Deserialize)]
struct Effects {
    writes: Vec<Access>,
    mmio_reads: Vec<Access>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq)]
struct Access {
    address: u32,
    size: u8,
    value: u32,
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

fn limit(product: &str) -> usize {
    let value = env::var("NATIVE_CHECKPOINT_DIFF_LIMIT")
        .unwrap_or_else(|_| panic!("{product}: direct Rust differential is UNVERIFIED; NATIVE_CHECKPOINT_DIFF_LIMIT is required"))
        .parse::<usize>()
        .unwrap_or_else(|_| panic!("{product}: NATIVE_CHECKPOINT_DIFF_LIMIT is invalid"));
    assert!(
        (1..=MAX_STEPS).contains(&value),
        "{product}: NATIVE_CHECKPOINT_DIFF_LIMIT must be 1..={MAX_STEPS}"
    );
    value
}

fn trace(path_env: &str, product: &str, limit: usize) -> Trace {
    let path = env::var(path_env).unwrap_or_else(|_| panic!("{product}: missing {path_env}"));
    let path = Path::new(&path);
    assert!(
        path.is_absolute() && path.is_file(),
        "{product}: trace is missing"
    );
    assert!(
        std::fs::metadata(path)
            .expect("trace metadata is unreadable")
            .len()
            <= MAX_TRACE_BYTES,
        "{product}: trace is too large"
    );
    let trace: Trace = serde_json::from_slice(&std::fs::read(path).expect("trace is unreadable"))
        .unwrap_or_else(|_| panic!("{product}: trace is invalid"));
    assert_eq!(trace.format_version, 2, "{product}: trace version");
    assert_eq!(trace.product, product, "{product}: trace product");
    assert_eq!(trace.limit, limit, "{product}: trace limit");
    assert_eq!(trace.steps.len(), limit, "{product}: trace is truncated");
    for state in &trace.steps {
        for access in state.writes.iter().chain(&state.mmio_reads) {
            assert!(
                matches!(access.size, 1 | 2 | 4),
                "{product}: trace access size"
            );
        }
    }
    trace
}

fn effect_mismatch(product: &str, step: usize, category: &str, address: u32) -> ! {
    panic!("{product}: step={step} category={category} address=0x{address:08x}")
}

fn compare_effects(
    product: &str,
    step: usize,
    category: &str,
    expected: &[Access],
    actual: &[GuestAccess],
) {
    for (wanted, got) in expected.iter().zip(actual) {
        if wanted.address != got.address || wanted.size != got.size || wanted.value != got.value {
            effect_mismatch(product, step, category, wanted.address);
        }
    }
    if let Some(wanted) = expected.get(actual.len()) {
        effect_mismatch(product, step, category, wanted.address);
    }
    if let Some(got) = actual.get(expected.len()) {
        effect_mismatch(product, step, category, got.address);
    }
}

fn is_mmio(access: &GuestAccess) -> bool {
    matches!(access.address >> 24, 0xEC | 0xFC)
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
    let limit = limit(product);
    let mut machine = machine(mstate_env, product);
    let oracle = trace(trace_env, product, limit);
    machine.board.set_guest_access_capture(true);
    let mut guest_writes = 0;
    let mut mmio_reads = 0;
    for step in 0..limit {
        if let Some(category) =
            movec_issue_at(&mut machine).expect("bus failure before instruction")
        {
            panic!("{product}: step={step} stop={category}");
        }
        // Do not classify the preflight opcode read or state-import writes as guest effects.
        machine.board.clear_guest_accesses();
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
        let writes = machine.board.take_guest_writes();
        compare_effects(
            product,
            step,
            "guest_write",
            &oracle.steps[step].writes,
            &writes,
        );
        guest_writes += writes.len();
        let reads: Vec<_> = machine
            .board
            .take_guest_reads()
            .into_iter()
            .filter(is_mmio)
            .collect();
        compare_effects(
            product,
            step,
            "guest_mmio_read",
            &oracle.steps[step].mmio_reads,
            &reads,
        );
        mmio_reads += reads.len();
    }
    if mmio_reads == 0 {
        println!("{product}: guest_writes={guest_writes} mmio_reads=0 unexercised");
    } else {
        println!("{product}: guest_writes={guest_writes} mmio_reads={mmio_reads} compared");
    }
}

#[test]
fn corrupted_oracle_guest_write_is_detected() {
    let expected = [Access {
        address: 0x4000_0000,
        size: 4,
        value: 1,
    }];
    let actual = [GuestAccess {
        address: 0x4000_0000,
        size: 4,
        value: 2,
    }];
    let result =
        std::panic::catch_unwind(|| compare_effects("test", 0, "guest_write", &expected, &actual));
    assert!(result.is_err());
}

#[test]
#[ignore = "requires provenance-generated local oracle traces"]
fn dt2_checkpoint_effects() {
    run("dt2", "DT2_CHECKPOINT_MSTATE", "DT2_CHECKPOINT_TRACE");
}

#[test]
#[ignore = "requires provenance-generated local oracle traces"]
fn dn2_checkpoint_effects() {
    run("dn2", "DN2_CHECKPOINT_MSTATE", "DN2_CHECKPOINT_TRACE");
}
