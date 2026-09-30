//! Local, bounded Oracle host-event replay; not autonomous Device scheduling.
//! Firmware-derived traces and card inputs are operator-checked and ignored.

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
use machine::{Board, CompletionPolicy, Machine, SemaphoreAddresses, Time, TimerPolicy};
use periph::dspi::Peer;
use serde_json::{Value, json};

struct Image(Mutex<File>, u64);
impl RandomAccessRead for Image {
    fn len(&self) -> u64 {
        self.1
    }
    fn read_at(&self, offset: u64, dest: &mut [u8]) -> usize {
        let Ok(mut file) = self.0.lock() else {
            return 0;
        };
        if file.seek(SeekFrom::Start(offset)).is_err() {
            return 0;
        }
        file.read(dest).unwrap_or(0)
    }
}

struct Wire(Rc<RefCell<Vec<Vec<u8>>>>, usize);
impl Peer for Wire {
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

fn output(name: &str) -> String {
    let path = input(name);
    let out = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../out")
        .canonicalize()
        .unwrap();
    assert!(
        Path::new(&path)
            .parent()
            .unwrap()
            .canonicalize()
            .unwrap()
            .starts_with(out),
        "{name} must stay under ignored out/"
    );
    path
}

fn hex_bytes(encoded: &str) -> Vec<u8> {
    assert!(encoded.len() <= 128 && encoded.len() % 2 == 0 && !encoded.is_empty());
    encoded
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

#[test]
#[ignore = "requires operator-checked local firmware, MSTATE, card and Oracle log"]
fn replay_oracle_host_events() {
    let reference: Value =
        serde_json::from_slice(&std::fs::read(input("DT2_NATIVE_EVENTS")).unwrap()).unwrap();
    let profile: Value =
        serde_json::from_slice(&std::fs::read(input("DT2_FRAME_PROFILE")).unwrap()).unwrap();
    assert_eq!(reference["format_version"], 1);
    assert!(reference["stepping"] == "counted" || reference["stepping"] == "fast-observed");
    assert_eq!(
        reference["identity"]["main_sha256"],
        profile["image_sha256"]
    );
    let state =
        machine::state::parse(&std::fs::read(input("DT2_AUTO_READY_MSTATE")).unwrap()).unwrap();
    assert_eq!(state.manifest["main_sha256"], profile["image_sha256"]);
    let card_file = File::open(input("DT2_AUTO_CARD_IMAGE")).unwrap();
    let card_bytes = card_file.metadata().unwrap().len();
    let card = Card::with_backing(
        DEFAULT_CAPACITY_BLOCKS,
        Some(Box::new(Image(Mutex::new(card_file), card_bytes))),
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
    let wanted: usize = env::var("DT2_NATIVE_FRAMES").unwrap().parse().unwrap();
    let limit: u64 = env::var("DT2_NATIVE_LIMIT").unwrap().parse().unwrap();
    assert!((1..=256).contains(&wanted) && (1..=100_000_000).contains(&limit));
    let frames = Rc::new(RefCell::new(Vec::new()));
    board.dma.peer = Box::new(Wire(Rc::clone(&frames), wanted));
    let mut m = Machine::new(Cpu::new(), board);
    m.apply_state(&state).unwrap();
    m.board.enable_oracle_sdram_faults();
    m.cpu.emac.mask = 0; // explicit Oracle-only calibration, not recovered Device state
    let status = 0xec03_802c;
    let mut mmio = state.mmio_forced.clone();
    let saved = mmio
        .get(&status)
        .copied()
        .unwrap_or_else(|| m.board.read32(status).unwrap());
    mmio.insert(status, saved | periph::dspi::SR_LINK_IDLE);
    m.board.install_forced_mmio(mmio).unwrap();
    let addr = |key: &str| -> u32 { profile[key].as_u64().unwrap().try_into().unwrap() };
    m.board.write32(addr("gate"), 0).unwrap();

    // The same Oracle-only idle-yield policy as auto_wire.rs. Timer/idle
    // events remain native-scheduled, not replayed: this mode isolates only
    // panel and frame-force stimuli and MUST NOT claim full event parity.
    let main = std::fs::read(input("DT2_MAIN_OS_BIN")).unwrap();
    let spins: HashSet<u32> = (0..main.len().saturating_sub(1))
        .step_by(2)
        .filter(|&o| main[o..o + 2] == [0x60, 0xfe])
        .map(|o| 0x4000_0400u32 + o as u32)
        .collect();
    assert!(spins.contains(&m.cpu.pc));
    let host: Vec<&Value> = reference["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "force" || e["kind"] == "feed")
        .collect();
    assert!(
        !host.is_empty()
            && host
                .windows(2)
                .all(|w| w[0]["clock"].as_u64() <= w[1]["clock"].as_u64())
    );
    let mut report = Vec::new();
    let mut done = 0u64;
    let mut next = 0usize;
    let mut idle_passes = 0u64;
    let mut idle_yields = 0u64;
    let mut force_count = 0u64;
    let mut seen = 0usize;
    let vector: u8 = addr("vector").try_into().unwrap();
    let level = periph::intc::VECTOR_BASE
        .iter()
        .enumerate()
        .find(|(_, first)| u16::from(vector) >= **first && u16::from(vector) < **first + 64)
        .and_then(|(i, first)| {
            let icr = m
                .board
                .read8(periph::intc::BASES[i] + 0x40 + u32::from(vector) - u32::from(*first))
                .unwrap()
                & 7;
            (icr != 0).then_some(icr)
        });
    while done < limit {
        assert_eq!(
            m.clock, done,
            "native instruction clock not one step per iteration"
        );
        while next < host.len() && host[next]["clock"].as_u64() == Some(done) {
            let event = host[next];
            let pre_pc = m.cpu.pc;
            let pre_sr = m.cpu.sr;
            if event["kind"] == "feed" {
                let bytes = hex_bytes(event["bytes_hex"].as_str().unwrap());
                let base = m.board.read32(addr("uart8_ring_ptr")).unwrap();
                let mut write = m.board.read32(0xfc04_5450).unwrap();
                assert!(write.wrapping_sub(base) <= 0x3ff);
                for byte in bytes {
                    m.board.write8(write, byte).unwrap();
                    write = base + ((write.wrapping_sub(base) + 1) & 0x3ff);
                }
                m.board.write32(0xfc04_5450, write).unwrap();
                assert!(
                    m.cpu
                        .take_interrupt(&mut m.board, 154, None, InterruptPolicy::Oracle)
                        .unwrap()
                );
            } else {
                m.board.write32(addr("counter"), 0).unwrap();
                assert!(
                    m.cpu
                        .take_interrupt(&mut m.board, vector, level, InterruptPolicy::Oracle)
                        .unwrap()
                );
                force_count += 1;
            }
            report.push(json!({"kind":event["kind"],"clock":done,"pre_pc":pre_pc,"pre_sr":pre_sr,
                "index":event["index"],"forced":if event["kind"] == "force" { Some(force_count) } else { None },
                "post_pc":m.cpu.pc,"source_pre_pc":event["pre_pc"],"source_post_pc":event["post_pc"]}));
            next += 1;
        }
        // The Python FrameForcer can defer at a PIT boundary; it does NOT
        // write a counter or raise a vector on a 'defer' record.
        let pc = m.cpu.pc;
        if spins.contains(&pc) {
            idle_passes += 1;
            if idle_passes.is_multiple_of(20_000) {
                assert!(
                    m.cpu
                        .take_interrupt(&mut m.board, 32, None, InterruptPolicy::Oracle)
                        .unwrap()
                );
                idle_yields += 1;
            }
        }
        m.step_timed().unwrap_or_else(|error| {
            panic!("native replay stopped after {done} at {pc:#x}: {error:?}")
        });
        done += 1;
        let count = frames.borrow().len();
        if count > seen {
            for index in seen..count {
                report.push(
                    json!({"kind":"tx","index":index,"clock":done,"after_force":force_count}),
                );
            }
            seen = count;
        }
        if count >= wanted {
            break;
        }
    }
    assert_eq!(
        frames.borrow().len(),
        wanted,
        "native replay stopped before requested frame count after {done} actual steps"
    );
    let mut wire = b"DTFR".to_vec();
    wire.extend_from_slice(&1u32.to_le_bytes());
    wire.extend_from_slice(&(wanted as u32).to_le_bytes());
    for frame in frames.borrow().iter() {
        wire.extend_from_slice(&(frame.len() as u32).to_le_bytes());
        wire.extend_from_slice(frame);
    }
    std::fs::write(output("DT2_NATIVE_WIRE"), wire).unwrap();
    std::fs::write(
        output("DT2_NATIVE_DIAG"),
        serde_json::to_vec_pretty(&json!({
            "format_version":1,"policy":"python-force-and-feed-replay/native-timer-and-idle",
            "source_dtfr_sha256":reference["dtfr_sha256"],"actual_native_instructions":done,
            "forces":force_count,"idle_yields":idle_yields,"events":report,
        }))
        .unwrap(),
    )
    .unwrap();
    println!(
        "native replay: {wanted} frames, {done} actual instructions, {force_count} host forces, {idle_yields} autonomous idle yields"
    );
}
