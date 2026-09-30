use std::collections::BTreeMap;

use coldfire::{Bus, Cpu};
use emmc_card::Card;
use machine::{
    Board, CompletionPolicy, Machine, MachineState, SemaphoreAddresses, StateApplyError, Time,
    TimedStepError, TimerPolicy,
    state::{PAGE_SIZE, Page, Registers},
};
use periph::{
    dtim::{BASES as DTIM_BASES, VECTORS as DTIM_VECTORS},
    intc::{BASES as INTC_BASES, VECTOR_BASE},
    pit::{BASES as PIT_BASES, F_BUS, VECTORS},
};

const RAM: u32 = 0x4000_0000;

fn state() -> MachineState {
    MachineState {
        clock: 17,
        regs: Registers {
            d: [1; 8],
            a: [2; 8],
            pc: RAM + 4,
            sr: 0x2000,
        },
        ctlregs: BTreeMap::from([
            (0x002, 3),
            (0x801, RAM),
            (0xc04, 0x1234),
            (0xc05, 0x1234),
            (0x999, 7),
            (0x80e, 0),
            (0x80f, 0),
        ]),
        mapped_bases: vec![RAM, RAM + PAGE_SIZE as u32],
        mmio_forced: BTreeMap::new(),
        ff1_count: 4,
        movec_count: 5,
        components: serde_json::Value::Null,
        manifest: serde_json::Value::Null,
        pages: vec![Page {
            base: RAM + PAGE_SIZE as u32,
            data: {
                let mut page = vec![0; PAGE_SIZE];
                page[3] = 0xa5;
                page
            },
        }],
        overlay_sectors: None,
    }
}

fn machine() -> Machine {
    Machine::new(
        Cpu::new(),
        Board::new(
            Card::default(),
            SemaphoreAddresses::default(),
            CompletionPolicy::Oracle,
        ),
    )
}

#[test]
fn apply_state_maps_zero_pages_and_keeps_known_and_unknown_ctlregs() {
    let mut machine = machine();
    machine.apply_state(&state()).unwrap();
    assert_eq!(machine.board.read32(RAM).unwrap(), 0);
    assert_eq!(
        machine.board.read8(RAM + PAGE_SIZE as u32 + 3).unwrap(),
        0xa5
    );
    assert_eq!(machine.cpu.ctrl.cacr, 3);
    assert_eq!(machine.cpu.ctrl.vbr, RAM);
    assert_eq!(machine.cpu.ctrl.rambar, 0x1234);
    assert_eq!(machine.unknown_ctlregs, BTreeMap::from([(0x999, 7)]));
    assert_eq!(machine.cpu.pc, RAM + 4);
    assert_eq!(machine.cpu.sr, 0x2000);
    assert_eq!(machine.clock, 17);
}

#[test]
fn imported_mstate_page_restores_dtim_registers_inside_the_one_megabyte_page() {
    let mut machine = machine();
    machine.board.attach_time(Time::with_dtims(
        TimerPolicy::Oracle,
        vec![],
        vec![3],
        F_BUS,
    ));
    let page_base = DTIM_BASES[3] & !(PAGE_SIZE as u32 - 1);
    let mut page = vec![0; PAGE_SIZE];
    let slot = (DTIM_BASES[3] - page_base) as usize;
    page[slot + 3] = 0x34;
    machine.board.import_ram_page(page_base, &page).unwrap();
    assert_eq!(machine.board.read8(DTIM_BASES[3] + 3).unwrap(), 0x34);
}

#[test]
fn apply_state_rejects_conflicting_rambar_aliases_component_and_invalid_maps_atomically() {
    let mut machine = machine();
    let mut aliases = state();
    aliases.ctlregs.insert(0xc05, 9);
    assert_eq!(
        machine.apply_state(&aliases),
        Err(StateApplyError::ConflictingRambarAliases {
            c04: 0x1234,
            c05: 9
        })
    );
    let mut components = state();
    components.components = serde_json::json!({"Pits": {"next": [1]}});
    assert_eq!(
        machine.apply_state(&components),
        Err(StateApplyError::UnsupportedComponents)
    );
    let mut invalid = state();
    invalid.mapped_bases[0] += 1;
    assert_eq!(
        machine.apply_state(&invalid),
        Err(StateApplyError::InvalidMappedInput)
    );
    assert_eq!(machine.cpu.pc, 0);
    assert!(machine.board.read8(RAM).is_err());
}

#[test]
fn apply_state_installs_big_endian_forced_read_words_without_changing_writes() {
    let mut machine = machine();
    let mut checkpoint = state();
    let forced = RAM + 0x100;
    checkpoint.mmio_forced.insert(forced, 0x1122_3344);
    machine.apply_state(&checkpoint).unwrap();
    assert_eq!(machine.board.read8(forced).unwrap(), 0x11);
    assert_eq!(machine.board.read16(forced + 1).unwrap(), 0x2233);
    assert_eq!(machine.board.read32(forced).unwrap(), 0x1122_3344);
    machine.board.write32(forced, 0xaabb_ccdd).unwrap();
    machine.board.write8(forced + 4, 0x55).unwrap();
    assert_eq!(machine.board.read32(forced).unwrap(), 0x1122_3344);
    assert_eq!(machine.board.read16(forced + 3).unwrap(), 0x4455);

    let edge = RAM + 2 * PAGE_SIZE as u32 - 4;
    machine
        .board
        .install_forced_mmio(BTreeMap::from([(edge, 0x1122_3344)]))
        .unwrap();
    assert!(machine.board.read16(edge + 3).is_err());

    let mut owned = state();
    // DTIM3 abuts PIT0, so their shared boundary is no longer unowned.
    owned.mmio_forced.insert(PIT_BASES[3] + 0x3fff, 1);
    assert_eq!(
        Machine::new(
            Cpu::new(),
            Board::new(
                Card::default(),
                SemaphoreAddresses::default(),
                CompletionPolicy::Oracle
            )
        )
        .apply_state(&owned),
        Err(StateApplyError::ForcedMmioOverlapsOwned)
    );
}

fn enable_pit0(time: &mut Time) {
    time.write(PIT_BASES[0], 2, 0x000b);
    time.write(PIT_BASES[0] + 2, 2, 0);
    let vector = VECTORS[0];
    let source = u32::from(vector - VECTOR_BASE[2]);
    let base = INTC_BASES[2];
    time.write(base + 0x40 + source, 1, 1);
    let mask = time.read(base + 0x0c, 4).unwrap();
    time.write(base + 0x0c, 4, mask & !(1 << source));
}

#[test]
fn timed_oracle_step_delivers_due_pit_at_boundary() {
    let mut machine = machine();
    machine.board.map_ram_page(RAM).unwrap();
    machine.board.write16(RAM, 0x4e71).unwrap(); // NOP
    machine
        .board
        .write32(RAM + 4 * u32::from(VECTORS[0]), RAM + 0x100)
        .unwrap();
    machine.cpu.pc = RAM;
    machine.cpu.ctrl.vbr = RAM;
    machine.cpu.a[7] = RAM + 0x8000;
    machine.cpu.sr = 0x2000;
    let mut time = Time::new(TimerPolicy::Oracle, vec![0], F_BUS);
    enable_pit0(&mut time);
    machine.board.attach_time(time);
    machine.step_timed().unwrap();
    assert_eq!(machine.clock, 1);
    assert_eq!(machine.cpu.pc, RAM + 0x100);
}

#[test]
fn timed_oracle_step_applies_dtim_ref_as_host_not_guest_write() {
    let mut machine = machine();
    machine.board.map_ram_page(RAM).unwrap();
    machine.board.write16(RAM, 0x4e71).unwrap(); // NOP
    machine.cpu.pc = RAM;
    machine.cpu.sr = 0x2000;
    let dtim3 = DTIM_BASES[3];
    let mut time = Time::with_dtims(TimerPolicy::Oracle, vec![], vec![3], F_BUS);
    time.write(dtim3, 2, 0x001b); // bus clock, period 1 instruction
    time.write(dtim3 + 4, 4, 0);
    machine.board.attach_time(time);
    machine.step_timed().unwrap();
    assert_eq!(machine.clock, 1);
    assert_eq!(machine.board.read8(dtim3 + 3).unwrap() & 0x02, 0x02);
    assert!(
        machine
            .board
            .time_mut()
            .unwrap()
            .take_host_writes()
            .is_empty()
    );
}

#[test]
fn timed_missing_dtim_handler_retains_pending_and_retries_once() {
    let mut machine = machine();
    machine.board.map_ram_page(RAM).unwrap();
    machine.board.write16(RAM, 0x4e71).unwrap();
    machine.board.write16(RAM + 2, 0x4e71).unwrap();
    machine.cpu.pc = RAM;
    machine.cpu.ctrl.vbr = RAM;
    machine.cpu.a[7] = RAM + 0x8000;
    machine.cpu.sr = 0x2000;
    let dtim3 = DTIM_BASES[3];
    let vector = DTIM_VECTORS[3];
    let mut time = Time::with_dtims(TimerPolicy::Oracle, vec![], vec![3], F_BUS);
    time.write(dtim3, 2, 0x001b); // period 1 instruction
    time.write(dtim3 + 4, 4, 0);
    let source = u32::from(vector - VECTOR_BASE[0]);
    let base = INTC_BASES[0];
    time.write(base + 0x40 + source, 1, 2);
    let mask = time.read(base + 0x08, 4).unwrap();
    time.write(base + 0x08, 4, mask & !(1 << (source % 32)));
    machine.board.attach_time(time);
    assert_eq!(
        machine.step_timed(),
        Err(TimedStepError::MissingOracleHandler {
            vector: vector as u8
        })
    );
    assert_eq!(machine.board.read8(dtim3 + 3).unwrap() & 0x02, 0x02);
    machine
        .board
        .write32(RAM + 4 * u32::from(vector), RAM + 0x100)
        .unwrap();
    machine.step_timed().unwrap();
    assert_eq!(machine.cpu.pc, RAM + 0x100);
    assert_eq!(machine.cpu.sr & 0x0700, 0x0200);
}

#[test]
fn timed_missing_handler_retains_pit_pending_for_retry() {
    let mut machine = machine();
    machine.board.map_ram_page(RAM).unwrap();
    machine.board.write16(RAM, 0x4e71).unwrap();
    machine.cpu.pc = RAM;
    machine.cpu.ctrl.vbr = RAM;
    machine.cpu.a[7] = RAM + 0x8000;
    machine.cpu.sr = 0x2000;
    let mut time = Time::new(TimerPolicy::Oracle, vec![0], F_BUS);
    enable_pit0(&mut time);
    machine.board.attach_time(time);
    assert_eq!(
        machine.step_timed(),
        Err(TimedStepError::MissingOracleHandler {
            vector: VECTORS[0] as u8
        })
    );
    machine
        .board
        .write32(RAM + 4 * u32::from(VECTORS[0]), RAM + 0x100)
        .unwrap();
    machine.step_timed().unwrap();
    assert_eq!(machine.cpu.pc, RAM + 0x100);
}

#[test]
fn timed_frame_preflight_rejects_mapped_first_word_and_retains_pending() {
    let mut machine = machine();
    machine.board.map_ram_page(RAM).unwrap();
    machine.board.write16(RAM, 0x4e71).unwrap();
    machine.board.write16(RAM + 2, 0x4e71).unwrap();
    machine
        .board
        .write32(RAM + 4 * u32::from(VECTORS[0]), RAM + 0x100)
        .unwrap();
    machine.cpu.pc = RAM;
    machine.cpu.ctrl.vbr = RAM;
    machine.cpu.a[7] = RAM + PAGE_SIZE as u32 + 4;
    machine.cpu.sr = 0x2000;
    let mut time = Time::new(TimerPolicy::Oracle, vec![0], F_BUS);
    enable_pit0(&mut time);
    machine.board.attach_time(time);
    assert_eq!(
        machine.step_timed(),
        Err(TimedStepError::InterruptFrameUnavailable {
            vector: VECTORS[0] as u8
        })
    );
    assert_eq!(machine.cpu.sr, 0x2000);
    assert_eq!(machine.cpu.a[7], RAM + PAGE_SIZE as u32 + 4);
    machine.board.map_ram_page(RAM + PAGE_SIZE as u32).unwrap();
    machine.step_timed().unwrap();
    assert_eq!(machine.cpu.pc, RAM + 0x100);
}

#[test]
fn timed_frame_preflight_uses_eusp_switched_stack() {
    let mut machine = machine();
    machine.board.map_ram_page(RAM).unwrap();
    machine.board.write16(RAM, 0x4e71).unwrap();
    machine
        .board
        .write32(RAM + 4 * u32::from(VECTORS[0]), RAM + 0x100)
        .unwrap();
    machine.cpu.pc = RAM;
    machine.cpu.ctrl.vbr = RAM;
    machine.cpu.a[7] = 0xdead_beef;
    machine.cpu.other_a7 = RAM + 0x800;
    machine.cpu.ctrl.cacr = 0x20;
    machine.cpu.sr = 0;
    let mut time = Time::new(TimerPolicy::Oracle, vec![0], F_BUS);
    enable_pit0(&mut time);
    machine.board.attach_time(time);
    machine.step_timed().unwrap();
    assert_eq!(machine.cpu.pc, RAM + 0x100);
    assert_eq!(machine.cpu.a[7], RAM + 0x7f8);
    assert_eq!(machine.cpu.other_a7, 0xdead_beef);
}

#[test]
fn timed_device_step_is_explicitly_unsupported() {
    let mut machine = machine();
    machine.board.map_ram_page(RAM).unwrap();
    machine.board.write16(RAM, 0x4e71).unwrap();
    machine.cpu.pc = RAM;
    machine
        .board
        .attach_time(Time::new(TimerPolicy::Device, vec![0], F_BUS));
    assert_eq!(
        machine.step_timed(),
        Err(TimedStepError::DeviceTimingUnsupported)
    );
}
