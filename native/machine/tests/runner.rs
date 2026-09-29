use coldfire::{Bus, Cpu, Stop, cpu::RunState};
use emmc_card::{Card, DEFAULT_CAPACITY_BLOCKS, RandomAccessRead};
use machine::{Board, CompletionEvent, CompletionPolicy, Machine, SemaphoreAddresses};
use periph::{edma, esdhc};

const BASE: u32 = 0x4000_0000;
const CODE: u32 = BASE + 0x1000;
const DEST: u32 = BASE + 0x2000;
const HANDLER: u32 = BASE + 0x3000;

struct SyntheticMedia;
impl RandomAccessRead for SyntheticMedia {
    fn len(&self) -> u64 {
        512
    }
    fn read_at(&self, offset: u64, dst: &mut [u8]) -> usize {
        let available = (512u64.saturating_sub(offset) as usize).min(dst.len());
        for (i, byte) in dst[..available].iter_mut().enumerate() {
            let pos = offset as usize + i;
            *byte = match pos {
                0 => 0x70,
                1 => 0x02,
                _ => pos as u8,
            };
        }
        available
    }
}

fn setup(policy: CompletionPolicy) -> Machine {
    let card = Card::with_backing(DEFAULT_CAPACITY_BLOCKS, Some(Box::new(SyntheticMedia))).unwrap();
    let mut board = Board::new(card, SemaphoreAddresses::default(), policy);
    board.map_ram_page(BASE).unwrap();
    let tcd = edma::TCD_BASE + 59 * 0x20;
    board.write32(tcd + edma::SADDR as u32, 0).unwrap();
    board.write32(tcd + edma::NBYTES as u32, 512).unwrap();
    board.write32(tcd + edma::DADDR as u32, DEST).unwrap();
    board.write16(tcd + edma::CITER as u32, 1).unwrap();
    board.write16(tcd + edma::BITER as u32, 1).unwrap();
    board
        .write16(tcd + edma::CSR as u32, edma::CSR_INT_MAJOR)
        .unwrap();
    // Both words were independently validated by cfdis; no firmware bytes.
    board.write16(CODE, 0x13c1).unwrap(); // MOVE.B D1,(SERQ).L
    board.write16(CODE + 2, 0xfc04).unwrap();
    board.write16(CODE + 4, 0x4018).unwrap();
    board.write16(CODE + 6, 0x23c0).unwrap(); // MOVE.L D0,(XFERTYP).L
    board.write16(CODE + 8, 0xfc0c).unwrap();
    board.write16(CODE + 10, 0xc00c).unwrap();
    let mut cpu = Cpu::new();
    cpu.pc = CODE;
    cpu.ctrl.vbr = BASE;
    cpu.a[7] = BASE + 0x8000;
    cpu.sr = 0x2000;
    cpu.d[0] = (18 << 24) | (1 << 21) | (1 << 4);
    cpu.d[1] = 59;
    Machine::new(cpu, board)
}

#[test]
fn guest_instructions_transfer_512_bytes_and_invalidate_decoded_ram() {
    let mut machine = setup(CompletionPolicy::Oracle);
    machine.board.write16(DEST, 0x7001).unwrap(); // MOVEQ #1,D0
    machine.cpu.pc = DEST;
    machine.cpu.step(&mut machine.board).unwrap(); // populate decode cache
    assert_eq!(machine.cpu.d[0], 1);
    machine.cpu.d[0] = (18 << 24) | (1 << 21) | (1 << 4);
    machine.cpu.pc = CODE;
    assert!(machine.step().unwrap().is_empty()); // guest SERQ59
    let effects = machine.step().unwrap(); // guest CMD18, DMA59
    // Normal stepping never retains an unbounded per-instruction access log.
    assert!(machine.board.take_guest_reads().is_empty());
    assert!(machine.board.take_guest_writes().is_empty());
    assert!(effects.iter().any(
        |event| matches!(event, CompletionEvent::Dma59 { completion, .. } if completion.done)
    ));
    assert_eq!(machine.clock, 2);
    assert_eq!(machine.board.read16(DEST).unwrap(), 0x7002);
    assert_eq!(machine.board.read8(DEST + 0xaf).unwrap(), 0xaf);
    assert!(machine.board.take_dma_written_ranges().is_empty());
    let tcd = edma::TCD_BASE + 59 * 0x20;
    assert_eq!(
        machine.board.read32(tcd + edma::DADDR as u32).unwrap(),
        DEST + 512
    );
    assert_eq!(machine.board.read16(tcd + edma::CITER as u32).unwrap(), 1);
    machine.cpu.pc = DEST;
    machine.step().unwrap(); // must fetch newly DMA-written MOVEQ #2
    assert_eq!(machine.cpu.d[0], 2);
    assert_eq!(machine.irqs_delivered, 0); // Oracle is not Device IRQ delivery
}

#[test]
fn device_major_completion_enters_caller_configured_handler() {
    let mut machine = setup(CompletionPolicy::Device);
    const VECTOR: u8 = 210; // synthetic caller choice, NOT a hardware eDMA vector
    machine
        .board
        .write32(BASE + 4 * u32::from(VECTOR), HANDLER)
        .unwrap();
    machine.board.write16(HANDLER, 0x4e73).unwrap(); // RTE
    machine.configure_dma_irq(Some((VECTOR, 3))).unwrap();
    assert!(machine.step().unwrap().is_empty());
    let effects = machine.step().unwrap();
    assert!(effects.iter().any(|event| matches!(event, CompletionEvent::Dma59 { policy: CompletionPolicy::Device, completion } if completion.done && completion.major_interrupt)));
    assert_eq!(machine.irqs_delivered, 0); // next instruction boundary
    assert_eq!(machine.cpu.pc, CODE + 12);
    machine.step().unwrap(); // enter IRQ, execute RTE, restore interrupted PC
    assert_eq!(machine.irqs_delivered, 1);
    assert_eq!(machine.cpu.pc, CODE + 12);
    assert_eq!(machine.cpu.sr, 0x2000);
    assert_eq!(machine.clock, 3);
}

#[test]
fn halted_step_retains_device_completion_events() {
    let mut machine = setup(CompletionPolicy::Device);
    machine.board.write_guest(edma::SERQ, 1, 59).unwrap();
    machine
        .board
        .write_guest(
            esdhc::BASE + esdhc::XFERTYP,
            4,
            (18 << 24) | (1 << 21) | (1 << 4),
        )
        .unwrap();
    let queued_events = machine.board.completion_events().to_vec();
    assert!(!queued_events.is_empty());

    machine.cpu.state = RunState::Halted;
    assert!(matches!(machine.step(), Err(Stop::Halted)));
    assert!(machine.board.completion_events().is_empty());
    assert_eq!(machine.held_completion_events(), queued_events);
    assert!(
        machine
            .held_completion_events()
            .iter()
            .any(|event| matches!(
                event,
                CompletionEvent::Dma59 {
                    policy: CompletionPolicy::Device,
                    completion,
                } if completion.done && completion.major_interrupt
            ))
    );
    assert!(machine.board.take_dma_written_ranges().is_empty());
}

#[test]
fn retry_after_failed_irq_handler_does_not_redeliver_same_completion() {
    let mut machine = setup(CompletionPolicy::Device);
    const VECTOR: u8 = 210;
    machine
        .board
        .write32(BASE + 4 * u32::from(VECTOR), HANDLER)
        .unwrap();
    machine.board.write16(HANDLER, 0x4acc).unwrap(); // PULSE
    machine.configure_dma_irq(Some((VECTOR, 3))).unwrap();
    machine.board.write_guest(edma::SERQ, 1, 59).unwrap();
    machine
        .board
        .write_guest(
            esdhc::BASE + esdhc::XFERTYP,
            4,
            (18 << 24) | (1 << 21) | (1 << 4),
        )
        .unwrap();

    machine.cpu.state = RunState::Halted;
    assert!(matches!(machine.step(), Err(Stop::Halted)));
    assert!(
        machine
            .held_completion_events()
            .iter()
            .any(|event| matches!(
                event,
                CompletionEvent::Dma59 {
                    policy: CompletionPolicy::Device,
                    completion,
                } if completion.done && completion.major_interrupt
            ))
    );

    machine.cpu.state = RunState::Running;
    assert!(matches!(machine.step(), Err(Stop::Unimplemented(..))));
    assert_eq!(machine.irqs_delivered, 1);

    machine.cpu.sr = 0x2000;
    assert!(matches!(machine.step(), Err(Stop::Unimplemented(..))));
    assert_eq!(machine.irqs_delivered, 1);
    assert!(
        machine
            .held_completion_events()
            .iter()
            .any(|event| matches!(
                event,
                CompletionEvent::Dma59 {
                    policy: CompletionPolicy::Device,
                    completion,
                } if completion.done && completion.major_interrupt
            ))
    );
}
