//! Local first-wire probe, not a provenance check or native audio gate.
//! Inputs and output are ignored, operator-checked artifacts under out/.

use std::{
    cell::RefCell,
    collections::HashSet,
    env,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
    rc::Rc,
    sync::Mutex,
};

use coldfire::{Bus, Cpu, InterruptPolicy};
use emmc_card::{Card, DEFAULT_CAPACITY_BLOCKS, RandomAccessRead};
use machine::board::GuestAccessKind;
use machine::{Board, CompletionPolicy, Machine, SemaphoreAddresses, Time, TimerPolicy};
use periph::dspi::Peer;
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Feed {
    at: u64,
    data: [u8; 2],
}

struct Image(Mutex<File>, u64);

impl RandomAccessRead for Image {
    fn len(&self) -> u64 {
        self.1
    }

    fn read_at(&self, offset: u64, destination: &mut [u8]) -> usize {
        let Ok(mut file) = self.0.lock() else {
            return 0;
        };
        if file.seek(SeekFrom::Start(offset)).is_err() {
            return 0;
        }
        file.read(destination).unwrap_or(0)
    }
}

struct WireFrames(Rc<RefCell<Vec<Vec<u8>>>>, usize);

impl Peer for WireFrames {
    fn exchange(&mut self, tx: &[u8]) -> Vec<u8> {
        let mut frames = self.0.borrow_mut();
        if frames.len() < self.1 {
            frames.push(tx.to_vec());
        }
        vec![0; tx.len()]
    }
}

fn input(name: &str) -> String {
    let path = env::var(name).unwrap_or_else(|_| panic!("missing {name}"));
    assert!(Path::new(&path).is_absolute(), "{name} must be absolute");
    path
}

fn check_cpu(cpu: &mut Cpu, sample: &serde_json::Value, step: u64) {
    cpu.resolve_nzv();
    assert_eq!(sample["step"].as_u64(), Some(step));
    let regs = &sample["regs"];
    for i in 0..8 {
        assert_eq!(
            u64::from(cpu.d[i]),
            regs["d"][i].as_u64().unwrap(),
            "D{i} diverged after {step} frame-IRQ instructions"
        );
        assert_eq!(
            u64::from(cpu.a[i]),
            regs["a"][i].as_u64().unwrap(),
            "A{i} diverged after {step} frame-IRQ instructions"
        );
    }
    assert_eq!(
        u64::from(cpu.pc),
        regs["pc"].as_u64().unwrap(),
        "PC at step {step}"
    );
    assert_eq!(
        u64::from(cpu.sr),
        regs["sr"].as_u64().unwrap(),
        "SR at step {step}"
    );
}

#[test]
#[ignore = "requires locally checked MSTATE/card/profile/reference; direct Rust input has no provenance"]
fn auto_ready_native_wire_prefix() {
    let state = machine::state::parse(&std::fs::read(input("DT2_AUTO_READY_MSTATE")).unwrap())
        .expect("portable state");
    let card_path = input("DT2_AUTO_CARD_IMAGE");
    let file = File::open(card_path).unwrap();
    let bytes = file.metadata().unwrap().len();
    let card = Card::with_backing(
        DEFAULT_CAPACITY_BLOCKS,
        Some(Box::new(Image(Mutex::new(file), bytes))),
    )
    .unwrap();
    let mut board = Board::new(
        card,
        SemaphoreAddresses::default(),
        CompletionPolicy::Oracle,
    );
    board.attach_time(Time::with_dtims(
        TimerPolicy::Oracle,
        vec![3, 2, 0],
        vec![3],
        132_000_000.0,
    ));
    let wanted: usize = env::var("DT2_NATIVE_FRAMES")
        .expect("explicit bounded wire-frame count")
        .parse()
        .unwrap();
    assert!((1..=68).contains(&wanted));
    let accepted = Rc::new(RefCell::new(Vec::new()));
    board.dma.peer = Box::new(WireFrames(Rc::clone(&accepted), wanted));
    let mut machine = Machine::new(Cpu::new(), board);
    machine.apply_state(&state).unwrap();
    machine.board.enable_oracle_sdram_faults();
    // Python snapshots omit ColdFire EMAC registers: a newly constructed
    // Unicorn instance reads zero from MASK, whereas Cpu::new uses the RM
    // reset value 0xffff_ffff. This is an explicit Oracle-only calibration,
    // not recovery of the Device register from the checkpoint.
    machine.cpu.emac.mask = 0;
    // The Python auto runner scans the checked MAIN OS for BRA.B -2 idle
    // sites. This explicit Oracle-only probe uses the same input bytes and
    // its default 20,000-pass yield; it does not infer a Device interrupt.
    let main = std::fs::read(input("DT2_MAIN_OS_BIN")).unwrap();
    let spins: HashSet<u32> = (0..main.len().saturating_sub(1))
        .step_by(2)
        .filter(|&offset| main[offset..offset + 2] == [0x60, 0xfe])
        .map(|offset| 0x4000_0400u32 + offset as u32)
        .collect();
    assert!(!spins.is_empty());
    assert!(
        spins.contains(&machine.cpu.pc),
        "ready PC is not a checked idle site"
    );
    // The accepted Python GUI run opened this gate and forced a frame
    // interrupt every 200,000 instructions. The profile is selected by
    // image hash outside Rust; this is Oracle stimulus, not Device timing.
    let profile: serde_json::Value =
        serde_json::from_slice(&std::fs::read(input("DT2_FRAME_PROFILE")).unwrap()).unwrap();
    let reference: serde_json::Value =
        serde_json::from_slice(&std::fs::read(input("DT2_AUTO_IRQ_REFERENCE")).unwrap()).unwrap();
    assert_eq!(reference["format_version"], 1);
    let gate_limit = reference["limit"].as_u64().unwrap();
    assert!((32..=60_000).contains(&gate_limit));
    assert_eq!(reference["every"], 1);
    assert_eq!(reference["image_sha256"], profile["image_sha256"]);
    assert_eq!(
        reference["samples"].as_array().unwrap().len(),
        gate_limit as usize + 1
    );
    let address = |key| -> u32 { profile[key].as_u64().unwrap().try_into().unwrap() };
    let vector: u8 = address("vector").try_into().unwrap();
    assert_eq!(reference["vector"].as_u64(), Some(u64::from(vector)));
    let level = if let Some((index, first)) = periph::intc::VECTOR_BASE
        .iter()
        .enumerate()
        .find(|(_, first)| u16::from(vector) >= **first && u16::from(vector) < **first + 64)
    {
        let icr = machine
            .board
            .read8(periph::intc::BASES[index] + 0x40 + u32::from(vector) - u32::from(*first))
            .unwrap()
            & 7;
        (icr != 0).then_some(icr)
    } else {
        None
    };
    assert_eq!(reference["level"].as_u64(), level.map(u64::from));
    machine.board.write32(address("gate"), 0).unwrap();
    // Python re-applies this polled DSPI2 status mock *after* restoring a
    // checkpoint. The saved forced-MMIO word has bit 31 but not bit 28.
    let status = 0xec03_802c;
    let mut mmio = state.mmio_forced.clone();
    let saved = mmio
        .get(&status)
        .copied()
        .unwrap_or_else(|| machine.board.read32(status).unwrap());
    mmio.insert(status, saved | periph::dspi::SR_LINK_IDLE);
    machine.board.install_forced_mmio(mmio).unwrap();

    let feeds: Vec<Feed> = if wanted > 18 {
        serde_json::from_slice(&std::fs::read(input("DT2_NATIVE_FEED")).unwrap()).unwrap()
    } else {
        Vec::new()
    };
    if wanted > 18 {
        assert_eq!(feeds.len(), 2);
        assert_eq!((feeds[0].at, feeds[0].data), (3_516_425, [0x23, 0x01]));
        assert_eq!((feeds[1].at, feeds[1].data), (10_552_659, [0x23, 0x00]));
    }

    let limit: u64 = env::var("DT2_NATIVE_LIMIT")
        .expect("explicit bounded instruction limit")
        .parse()
        .unwrap();
    assert!((1..=20_000_000).contains(&limit));
    let mut done = 0;
    let mut idle_passes = 0u64;
    let mut idle_yields = 0u64;
    let mut forced = 0u64;
    let mut due = 0u64;
    let mut pending_force = false;
    let mut gate_step = 0u64;
    let mut handler_hits = 0u64;
    let mut driver_hits = 0u64;
    let mut checked_effects = 0usize;
    let mut first_at = None;
    let mut fed = 0usize;
    for _ in 0..limit {
        if fed < feeds.len() && done == feeds[fed].at {
            println!(
                "panel RX {fed} at {done}: pre-PC {:#x}, SR {:#x}",
                machine.cpu.pc, machine.cpu.sr
            );
            let base = machine.board.read32(address("uart8_ring_ptr")).unwrap();
            let mut write = machine.board.read32(0xfc04_5450).unwrap();
            assert!(
                write.wrapping_sub(base) <= 0x3ff,
                "UART8 RX ring pointer out of bounds"
            );
            for byte in feeds[fed].data {
                machine.board.write8(write, byte).unwrap();
                write = base + ((write.wrapping_sub(base) + 1) & 0x3ff);
            }
            machine.board.write32(0xfc04_5450, write).unwrap();
            assert!(
                machine
                    .cpu
                    .take_interrupt(&mut machine.board, 154, None, InterruptPolicy::Oracle)
                    .expect("Oracle panel RX handler")
            );
            println!(
                "panel RX {fed}: ring {base:#x}/{write:#x}, handler {:#x}",
                machine.cpu.pc
            );
            fed += 1;
        }
        if pending_force {
            machine.board.write32(address("counter"), 0).unwrap();
            assert!(
                machine
                    .cpu
                    .take_interrupt(&mut machine.board, vector, level, InterruptPolicy::Oracle)
                    .expect("Oracle forced frame handler")
            );
            forced += 1;
            due = machine.clock + 200_000;
            pending_force = false;
            if forced <= 2 || (50..=55).contains(&forced) {
                println!("NATIVE_FORCE {forced} at {done} PC {:#x}", machine.cpu.pc);
            }
            if forced == 1 {
                // The Python fast idle-code hook credits the host vector
                // before the first handler instruction; native steps the
                // vector entry independently. Keep this skew explicit.
                assert!(done.abs_diff(reference["force_at"].as_u64().unwrap()) <= 1);
            }
        }
        let pc = machine.cpu.pc;
        let checking = forced > 0 && gate_step < gate_limit;
        if checking {
            check_cpu(
                &mut machine.cpu,
                &reference["samples"][gate_step as usize],
                gate_step,
            );
            machine.board.set_guest_access_capture(true);
        }
        if pc == address("handler") {
            handler_hits += 1;
        }
        if pc == address("driver") {
            driver_hits += 1;
        }
        if spins.contains(&pc) {
            idle_passes += 1;
            if idle_passes.is_multiple_of(20_000) {
                assert!(
                    machine
                        .cpu
                        .take_interrupt(&mut machine.board, 32, None, InterruptPolicy::Oracle)
                        .expect("Oracle idle yield handler")
                );
                idle_yields += 1;
            }
        }
        let timer_deadline = if machine.clock >= due {
            let mut time = machine.board.take_time().unwrap();
            let deadline = time.deadline(machine.clock);
            machine.board.restore_time(time);
            deadline
        } else {
            None
        };
        machine.step_timed().unwrap_or_else(|error| {
            panic!("native stop after {done} instructions at PC {pc:#x}, forced {forced}, idle yields {idle_yields}: {error:?}")
        });
        // Python's GUI invokes FrameForcer.on_chunk only after a scheduler
        // deadline or its fast stepper stops on entry to an idle spin. A
        // literal 0/200k/400k host schedule shifts frames ahead of the
        // accepted GUI by thousands of instructions after each boundary.
        if (timer_deadline.is_some_and(|deadline| deadline <= machine.clock)
            || (!spins.contains(&pc) && spins.contains(&machine.cpu.pc)))
            && machine.clock >= due
            && level.is_none_or(|n| (machine.cpu.sr >> 8) & 7 < u16::from(n))
        {
            pending_force = true;
        }
        if checking {
            let actual = machine.board.take_guest_accesses();
            let effects = reference["effects"].as_array().unwrap();
            let expected: Vec<_> = effects
                .iter()
                .filter(|event| event["step"].as_u64() == Some(gate_step))
                .collect();
            let actual: Vec<_> = actual
                .iter()
                .filter(|event| {
                    (event.kind == GuestAccessKind::Write && event.access.address < 0x8000_0000)
                        || (event.access.address >= 0x8c00_0000
                            && (event.access.address <= 0x8fff_ffff
                                || event.access.address >= 0xc000_0000))
                })
                .collect();
            assert_eq!(
                actual.len(),
                expected.len(),
                "guest effect count at frame-IRQ step {gate_step}"
            );
            for (index, (access, event)) in actual.iter().zip(expected).enumerate() {
                let kind = match access.kind {
                    GuestAccessKind::Read => "RD",
                    GuestAccessKind::Write if access.access.address < 0x8000_0000 => "RAM_WR",
                    GuestAccessKind::Write => "WR",
                };
                assert_eq!(
                    event["kind"], kind,
                    "effect {index} at IRQ step {gate_step}"
                );
                assert_eq!(
                    event["pc"].as_u64(),
                    Some(u64::from(pc)),
                    "effect PC {index} at IRQ step {gate_step}"
                );
                assert_eq!(
                    event["address"].as_u64(),
                    Some(u64::from(access.access.address)),
                    "effect address {index} at IRQ step {gate_step}"
                );
                assert_eq!(
                    event["size"].as_u64(),
                    Some(u64::from(access.access.size)),
                    "effect size {index} at IRQ step {gate_step}"
                );
                if let Some(value) = event["value"].as_u64() {
                    assert_eq!(
                        value,
                        u64::from(access.access.value),
                        "effect value {index} at IRQ step {gate_step}"
                    );
                }
                checked_effects += 1;
            }
            gate_step += 1;
            if gate_step == gate_limit {
                check_cpu(
                    &mut machine.cpu,
                    &reference["samples"][gate_limit as usize],
                    gate_limit,
                );
                assert_eq!(checked_effects, effects.len());
                machine.board.set_guest_access_capture(false);
            }
        }
        done += 1;
        if first_at.is_none() && accepted.borrow().len() == 1 {
            first_at = Some(done);
        }
        if accepted.borrow().len() >= wanted && gate_step >= gate_limit {
            break;
        }
    }
    let frame = accepted.borrow();
    let tcd29 = periph::edma::TCD_BASE + 29 * 0x20;
    let citer = machine
        .board
        .read16(tcd29 + periph::edma::CITER as u32)
        .unwrap();
    let nbytes = machine
        .board
        .read32(tcd29 + periph::edma::NBYTES as u32)
        .unwrap();
    assert!(
        !frame.is_empty(),
        "no native wire frame after {done} instructions; PC {:#x}, forced {forced}, handler {handler_hits}, driver {driver_hits}, DMA2 frames {}, TX bytes {}, TCD29 CITER {citer:#x}, NBYTES {nbytes:#x}, idle passes {idle_passes}, idle yields {idle_yields}",
        machine.cpu.pc,
        machine.board.dma.dspi2.frames,
        machine.board.dma.dspi2.tx_bytes
    );
    assert_eq!(frame.len(), wanted, "incomplete bounded wire-frame prefix");
    let output = input("DT2_NATIVE_WIRE");
    let ignored = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("out")
        .canonicalize()
        .unwrap();
    assert!(
        Path::new(&output)
            .parent()
            .unwrap()
            .canonicalize()
            .unwrap()
            .starts_with(&ignored),
        "wire trace must stay under ignored out/"
    );
    let mut wire = b"DTFR".to_vec();
    wire.extend_from_slice(&1u32.to_le_bytes());
    wire.extend_from_slice(&(wanted as u32).to_le_bytes());
    for tx in frame.iter() {
        wire.extend_from_slice(&(tx.len() as u32).to_le_bytes());
        wire.extend_from_slice(tx);
    }
    std::fs::write(output, wire).unwrap();
    println!(
        "native accepted first {}-byte wire frame after {} instructions; {wanted} ordered frames after {done} instructions, {forced} forced frame IRQs, {fed} panel RX deliveries and {idle_yields} idle yields",
        frame[0].len(),
        first_at.unwrap(),
    );
    println!(
        "matched {} CPU boundaries and {checked_effects} guest effects across first forced IRQ",
        gate_limit + 1,
    );
    println!(
        "last native PC {:#x}, SR {:#x}, {done} actual CPU steps",
        machine.cpu.pc, machine.cpu.sr
    );
}
