//! Firmware-free proof of the isolated, explicitly initialized eSDHC path.
use coldfire::{Bus, Cpu};
use emmc_card::{Card, CardIdentity};
use machine::runner::EsdhcStepError;
use machine::{Board, CompletionPolicy, Machine, SemaphoreAddresses, Time, TimerPolicy};
use periph::{edma, esdhc, intc};

const RAM: u32 = 0x4000_0000;
const CODE: u32 = RAM + 0x1000;
const HANDLER: u32 = RAM + 0x2000;
const INTC: u32 = intc::BASES[2];
const CC: u32 = 1;

fn setup(policy: CompletionPolicy, timer: Option<TimerPolicy>) -> Machine {
    let card = Card::with_backing(CardIdentity::Mlc4G, None).unwrap();
    let mut board = Board::new(
        card,
        SemaphoreAddresses {
            dma_sem: Some(RAM + 0x4000),
            data_sem: Some(RAM + 0x4100),
            cmd_sem: Some(RAM + 0x4200),
            status: Some(RAM + 0x4300),
        },
        policy,
    );
    board.map_ram_page(RAM).unwrap();
    for off in (0..32).step_by(2) {
        board.write16(CODE + off, 0x4e71).unwrap(); // NOP
    }
    board.write32(RAM + 223 * 4, HANDLER).unwrap();
    board.write16(HANDLER, 0x4e73).unwrap(); // RTE without acknowledgement
    if let Some(timer) = timer {
        board.attach_time(Time::new(timer, vec![], 1_000_000.0));
        // Explicit initialization: no reset or CALL/SALL behavior is assumed.
        board.write32(INTC + 0x0c, u32::MAX).unwrap();
        board.write8(INTC + 0x5f, 3).unwrap();
        board.write8(INTC + 0x1d, 31).unwrap();
    }
    board.write32(esdhc::BASE + esdhc::IRQSTATEN, CC).unwrap();
    board.write32(esdhc::BASE + esdhc::IRQSIGEN, CC).unwrap();
    let mut cpu = Cpu::new();
    cpu.pc = CODE;
    cpu.sr = 0x2000;
    cpu.a[7] = RAM + 0x8000;
    cpu.ctrl.vbr = RAM;
    Machine::new(cpu, board)
}

fn device() -> Machine {
    setup(CompletionPolicy::Device, Some(TimerPolicy::Device))
}

fn latch(machine: &mut Machine) {
    machine.board.esdhc.device_event(CC).unwrap();
}

#[test]
fn signal_imr_and_ipl_postpone_without_losing_latched_status() {
    let mut m = device();
    m.board.write32(esdhc::BASE + esdhc::IRQSIGEN, 0).unwrap();
    latch(&mut m);
    m.step_esdhc().unwrap();
    assert_eq!(m.irqs_delivered, 0);
    assert_eq!(m.board.read32(esdhc::BASE + esdhc::IRQSTAT).unwrap(), CC);
    m.board.write32(esdhc::BASE + esdhc::IRQSIGEN, CC).unwrap();
    m.board.write8(INTC + 0x1c, 31).unwrap(); // SIMR masks real IMR bit
    m.step_esdhc().unwrap();
    assert_eq!(m.irqs_delivered, 0);
    m.board.write8(INTC + 0x1d, 31).unwrap(); // CIMR unmasks source31
    assert_eq!(m.board.time().unwrap().esdhc_interrupt_level(), Some(3));
    assert_eq!(m.board.read32(INTC + 0x0c).unwrap(), 0x7fff_ffff);
    m.cpu.sr = 0x2300;
    m.step_esdhc().unwrap();
    assert_eq!(m.irqs_delivered, 0);
    m.cpu.sr = 0x2000;
    let resume = m.cpu.pc;
    m.step_esdhc().unwrap(); // interrupt entry and RTE
    assert_eq!(m.irqs_delivered, 1);
    assert_eq!(m.cpu.pc, resume);
    assert_eq!(m.cpu.sr, 0x2000);
    assert_eq!(m.cpu.a[7], RAM + 0x8000);
    assert!(m.board.esdhc.interrupt_pending());
}

#[test]
fn status_enable_and_zero_icr_disable_delivery() {
    let mut m = device();
    m.board.write32(esdhc::BASE + esdhc::IRQSTATEN, 0).unwrap();
    latch(&mut m);
    m.step_esdhc().unwrap();
    assert_eq!(m.irqs_delivered, 0);
    assert!(!m.board.esdhc.interrupt_pending());
    m.board.write32(esdhc::BASE + esdhc::IRQSTATEN, CC).unwrap();
    latch(&mut m);
    m.board.write8(INTC + 0x5f, 0).unwrap();
    m.step_esdhc().unwrap();
    assert_eq!(m.irqs_delivered, 0);
    assert!(m.board.esdhc.interrupt_pending());
    m.board.write8(INTC + 0x5f, 3).unwrap();
    m.step_esdhc().unwrap();
    assert_eq!(m.irqs_delivered, 1);
}

#[test]
fn unacknowledged_line_redelivers_after_rte() {
    let mut m = device();
    latch(&mut m);
    m.step_esdhc().unwrap();
    m.step_esdhc().unwrap();
    assert_eq!(m.irqs_delivered, 2);
    assert_eq!(m.cpu.pc, CODE);
}

#[test]
fn guest_irqstat_w1c_deasserts_line_before_rte() {
    let mut m = device();
    // MOVE.L D0,(IRQSTAT).L; RTE. These are synthetic instructions.
    m.board.write16(HANDLER, 0x23c0).unwrap();
    m.board
        .write32(HANDLER + 2, esdhc::BASE + esdhc::IRQSTAT)
        .unwrap();
    m.board.write16(HANDLER + 6, 0x4e73).unwrap();
    m.cpu.d[0] = CC;
    latch(&mut m);
    m.step_esdhc().unwrap();
    assert_eq!(m.cpu.pc, HANDLER + 6);
    assert_eq!(m.cpu.sr, 0x2300);
    assert!(!m.board.esdhc.interrupt_pending());
    m.step_esdhc().unwrap(); // RTE
    m.step_esdhc().unwrap(); // main NOP
    assert_eq!(m.irqs_delivered, 1);
    assert_eq!(m.cpu.pc, CODE + 2);
}

fn assert_rejected_unchanged(m: &mut Machine, expected: EsdhcStepError) {
    let mut before = periph::snap::Writer::new();
    m.board.snap_save(&mut before).unwrap();
    let regs = (
        m.cpu.pc,
        m.cpu.sr,
        m.cpu.a,
        m.cpu.d,
        m.clock,
        m.irqs_delivered,
    );
    let events = m.board.completion_events().to_vec();
    let held = m.held_completion_events().to_vec();
    assert_eq!(m.step_esdhc(), Err(expected));
    let mut after = periph::snap::Writer::new();
    m.board.snap_save(&mut after).unwrap();
    assert_eq!(before.buf, after.buf);
    assert_eq!(
        regs,
        (
            m.cpu.pc,
            m.cpu.sr,
            m.cpu.a,
            m.cpu.d,
            m.clock,
            m.irqs_delivered
        )
    );
    assert_eq!(m.board.completion_events(), events);
    assert_eq!(m.held_completion_events(), held);
}

#[test]
fn invalid_prerequisites_are_rejected_before_mutation() {
    assert_rejected_unchanged(
        &mut setup(CompletionPolicy::Oracle, Some(TimerPolicy::Device)),
        EsdhcStepError::DeviceCompletionRequired,
    );
    assert_rejected_unchanged(
        &mut setup(CompletionPolicy::Device, None),
        EsdhcStepError::TimerNotAttached,
    );
    assert_rejected_unchanged(
        &mut setup(CompletionPolicy::Device, Some(TimerPolicy::Oracle)),
        EsdhcStepError::DeviceTimerRequired,
    );
    let mut m = device();
    latch(&mut m);
    m.configure_dma_irq(Some((210, 3))).unwrap();
    assert_rejected_unchanged(&mut m, EsdhcStepError::LegacyDmaIrqConfigured);
}

#[test]
fn pending_dma_is_rejected_even_when_legacy_route_is_disabled() {
    let mut m = device();
    let tcd = edma::TCD_BASE + 59 * 0x20;
    m.board.write32(tcd + edma::NBYTES as u32, 512).unwrap();
    m.board
        .write32(tcd + edma::DADDR as u32, RAM + 0x3000)
        .unwrap();
    m.board.write16(tcd + edma::CITER as u32, 1).unwrap();
    m.board.write16(tcd + edma::BITER as u32, 1).unwrap();
    m.board
        .write16(tcd + edma::CSR as u32, edma::CSR_INT_MAJOR)
        .unwrap();
    m.board.write_guest(edma::SERQ, 1, 59).unwrap();
    m.board
        .write_guest(
            esdhc::BASE + esdhc::XFERTYP,
            4,
            (18 << 24) | (1 << 21) | (1 << 4),
        )
        .unwrap();
    // Queued major completion is competing DMA even before ordinary step has
    // transferred it into the runner's pending latch.
    let events = m.board.completion_events().to_vec();
    let pc = m.cpu.pc;
    let sr = m.cpu.sr;
    let status = m.board.read32(esdhc::BASE + esdhc::IRQSTAT).unwrap();
    assert_eq!(m.step_esdhc(), Err(EsdhcStepError::LegacyDmaIrqPending));
    assert_eq!(m.board.completion_events(), events);
    assert_eq!(
        (m.cpu.pc, m.cpu.sr, m.clock, m.irqs_delivered),
        (pc, sr, 0, 0)
    );
    assert_eq!(
        m.board.read32(esdhc::BASE + esdhc::IRQSTAT).unwrap(),
        status
    );
    m.step().unwrap(); // preserves existing event-to-pending-DMA behavior
    assert_rejected_unchanged(&mut m, EsdhcStepError::LegacyDmaIrqPending);
}

#[test]
fn ordinary_step_remains_opt_out_and_oracle_masks_remain_passthrough() {
    let mut m = device();
    latch(&mut m);
    m.step().unwrap();
    assert_eq!(m.irqs_delivered, 0);
    assert_eq!(m.cpu.pc, CODE + 2);
    let mut oracle = setup(CompletionPolicy::Oracle, Some(TimerPolicy::Oracle));
    assert_eq!(oracle.board.read32(INTC + 0x0c).unwrap(), u32::MAX);
    assert_eq!(oracle.board.read8(INTC + 0x1d).unwrap(), 31);
    oracle.board.write8(INTC + 0x1c, 31).unwrap();
    assert_eq!(oracle.board.read32(INTC + 0x0c).unwrap(), u32::MAX);
}

#[test]
fn device_command_irq_does_not_post_host_semaphores() {
    let mut m = device();
    m.board
        .write_guest(esdhc::BASE + esdhc::XFERTYP, 4, 13 << 24)
        .unwrap();
    assert!(m.board.esdhc.interrupt_pending());
    m.step_esdhc().unwrap();
    assert_eq!(m.irqs_delivered, 1);
    for address in [RAM + 0x4000, RAM + 0x4100, RAM + 0x4200, RAM + 0x4300] {
        assert_eq!(m.board.read32(address).unwrap(), 0);
    }
}

#[test]
fn failed_exception_entry_is_reported_as_poisoned() {
    let mut m = device();
    latch(&mut m);
    m.cpu.a[7] = 0; // no RAM for the exception frame
    assert!(matches!(
        m.step_esdhc(),
        Err(EsdhcStepError::InterruptDeliveryPoisoned { .. })
    ));
    assert_eq!(m.irqs_delivered, 0);
    assert_eq!(m.clock, 0);
}
