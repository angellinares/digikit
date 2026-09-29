//! Opt-in bounded native checkpoint gate. Fixtures are local ignored artifacts.

use std::{env, path::Path};

use coldfire::{Bus, Cpu, Stop};
use emmc_card::Card;
use machine::{Board, CompletionPolicy, Machine, SemaphoreAddresses, state::parse};

const UNVERIFIED_SMOKE_ACK: &str = "unverified-local-inputs";

fn stop_category(stop: Stop) -> &'static str {
    match stop {
        Stop::Halted => "halted",
        Stop::Unimplemented(_) => "unimplemented",
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
    let known = matches!(
        selector,
        0x002 | 0x003 | 0x004..=0x009 | 0x00c..=0x00f | 0x800 | 0x801 | 0x80e | 0x80f | 0xc04 | 0xc05
    );
    Ok((!known).then_some("unsupported_movec_unknown_write"))
}

fn run_checkpoint(product: &str, path_env: &str) {
    assert_eq!(
        env::var("NATIVE_CHECKPOINT_UNVERIFIED_SMOKE_ACK").as_deref(),
        Ok(UNVERIFIED_SMOKE_ACK),
        "{product}: direct Rust checkpoint test is unverified; set NATIVE_CHECKPOINT_UNVERIFIED_SMOKE_ACK={UNVERIFIED_SMOKE_ACK}"
    );
    let path = env::var(path_env).unwrap_or_else(|_| panic!("{product}: missing {path_env}"));
    let path = Path::new(&path);
    assert!(path.is_absolute(), "{product}: {path_env} must be absolute");
    assert!(path.is_file(), "{product}: checkpoint fixture is missing");
    let bytes = std::fs::read(path).expect("checkpoint fixture is unreadable");
    let state = parse(&bytes).expect("checkpoint MSTATE is invalid");
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
    for instruction in 0..1000_u64 {
        if let Some(category) =
            movec_issue_at(&mut machine).expect("bus failure before instruction")
        {
            panic!("{product}: instructions={instruction} stop={category}");
        }
        let events = match machine.step() {
            Ok(events) => events,
            Err(stop) => panic!(
                "{product}: instructions={instruction} stop={}",
                stop_category(stop)
            ),
        };
        if let Some(event) = events.first() {
            let category = match event {
                machine::CompletionEvent::Dma59 { .. } => "completion_dma59",
                machine::CompletionEvent::Data { .. } => "completion_data",
                machine::CompletionEvent::Command { .. } => "completion_command",
            };
            panic!("{product}: instructions={instruction} stop={category}");
        }
    }
    eprintln!("checkpoint-smoke-unverified product={product} instructions=1000 outcome=budget");
}

#[test]
#[ignore = "direct Rust smoke test requires explicit unverified-local-inputs acknowledgement"]
fn dt2_boot24m_checkpoint_gate() {
    run_checkpoint("dt2", "DT2_CHECKPOINT_MSTATE");
}

#[test]
#[ignore = "direct Rust smoke test requires explicit unverified-local-inputs acknowledgement"]
fn dn2_boot24m_checkpoint_gate() {
    run_checkpoint("dn2", "DN2_CHECKPOINT_MSTATE");
}
