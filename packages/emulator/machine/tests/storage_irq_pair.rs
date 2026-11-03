//! Firmware-free, bounded proof of the explicit INTC2 storage pair.
use coldfire::{Bus, Cpu};
use emmc_card::{Card, CardIdentity};
use machine::runner::StorageStepError;
use machine::{
    Board, CompletionEvent, CompletionPolicy, Machine, SemaphoreAddresses, Time, TimerPolicy,
};
use periph::{edma, esdhc, intc};

const RAM: u32 = 0x4000_0000;
const CODE: u32 = RAM + 0x1000;
const SD_HANDLER: u32 = RAM + 0x2000;
const DMA_HANDLER: u32 = RAM + 0x2100;
const DEST: u32 = RAM + 0x3000;
const INTC: u32 = intc::BASES[2];
const DMA59: u32 = 1 << 27;

fn setup(policy: CompletionPolicy, timer: Option<TimerPolicy>) -> Machine {
    let mut board = Board::new(
        Card::with_backing(CardIdentity::Mlc4G, None).unwrap(),
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
        board.write16(CODE + off, 0x4e71).unwrap();
    }
    for (vector, handler) in [(223, SD_HANDLER), (192, DMA_HANDLER)] {
        board.write32(RAM + vector * 4, handler).unwrap();
        board.write16(handler, 0x4e71).unwrap(); // NOP; RTE on next boundary
        board.write16(handler + 2, 0x4e73).unwrap();
    }
    if let Some(timer) = timer {
        board.attach_time(Time::new(timer, vec![], 1_000_000.0));
        board.write32(INTC + 0x0c, u32::MAX).unwrap();
        for source in [0, 31] {
            board.write8(INTC + 0x40 + source, 3).unwrap();
            board.write8(INTC + 0x1d, source as u8).unwrap();
        }
    }
    board.write32(esdhc::BASE + esdhc::IRQSTATEN, 3).unwrap();
    board.write32(esdhc::BASE + esdhc::IRQSIGEN, 3).unwrap();
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

fn read_dma(m: &mut Machine, interrupt: bool) {
    let tcd = edma::TCD_BASE + 59 * 0x20;
    m.board.write32(tcd + edma::NBYTES as u32, 512).unwrap();
    m.board.write32(tcd + edma::DADDR as u32, DEST).unwrap();
    m.board.write16(tcd + edma::CITER as u32, 1).unwrap();
    m.board.write16(tcd + edma::BITER as u32, 1).unwrap();
    m.board
        .write16(
            tcd + edma::CSR as u32,
            if interrupt { edma::CSR_INT_MAJOR } else { 0 },
        )
        .unwrap();
    m.board.write32(DEST, 0xdead_beef).unwrap();
    m.board.write8(edma::SERQ, 59).unwrap();
    m.board
        .write32(
            esdhc::BASE + esdhc::XFERTYP,
            (18 << 24) | (1 << 21) | (1 << 4),
        )
        .unwrap();
}

fn drain(m: &mut Machine) {
    m.board.take_completion_events();
    m.board.take_dma_written_ranges();
}

fn pending(m: &mut Machine) -> u32 {
    m.board.read32(edma::INTH).unwrap()
}

#[test]
fn actual_copy_latches_tc_and_dma_then_equal_level_sd_wins() {
    let mut m = device();
    read_dma(&mut m, true);
    assert_eq!(m.board.read32(DEST).unwrap(), 0); // unbacked card reads zero
    assert_eq!(pending(&mut m), DMA59);
    assert_eq!(m.board.read32(esdhc::BASE + esdhc::IRQSTAT).unwrap() & 3, 3);
    let events = m.step_storage().unwrap(); // accepts queued board completion
    assert!(events.iter().any(|e| matches!(e, CompletionEvent::Dma59 { completion, .. } if completion.done && completion.major_interrupt)));
    assert_eq!(m.cpu.pc, SD_HANDLER + 2);
    m.board.write32(esdhc::BASE + esdhc::IRQSTAT, 3).unwrap();
    m.step_storage().unwrap(); // RTE
    m.step_storage().unwrap();
    assert_eq!(m.cpu.pc, DMA_HANDLER + 2);
    assert_eq!(m.irqs_delivered, 2);
    assert_eq!(pending(&mut m), DMA59); // entry does not acknowledge
    for addr in [RAM + 0x4000, RAM + 0x4100, RAM + 0x4200, RAM + 0x4300] {
        assert_eq!(m.board.read32(addr).unwrap(), 0);
    }
}

#[test]
fn higher_dma_level_wins_and_unacknowledged_request_repeats_after_rte() {
    let mut m = device();
    read_dma(&mut m, true);
    m.board.write8(INTC + 0x40, 4).unwrap();
    m.step_storage().unwrap();
    assert_eq!(m.cpu.pc, DMA_HANDLER + 2);
    m.step_storage().unwrap(); // RTE
    m.step_storage().unwrap();
    assert_eq!(m.cpu.pc, DMA_HANDLER + 2);
    assert_eq!(m.irqs_delivered, 2);
    m.board.write8(edma::CINT, 59).unwrap();
    m.step_storage().unwrap();
    m.step_storage().unwrap();
    assert_eq!(m.cpu.pc, SD_HANDLER + 2);
}

#[test]
fn mask_icr_and_cpu_ipl_defer_without_losing_dma_latch() {
    let mut m = device();
    read_dma(&mut m, true);
    m.board.write32(esdhc::BASE + esdhc::IRQSIGEN, 0).unwrap();
    m.board.write8(INTC + 0x1c, 0).unwrap();
    m.step_storage().unwrap();
    assert_eq!(m.irqs_delivered, 0);
    m.board.write8(INTC + 0x1d, 0).unwrap();
    m.board.write8(INTC + 0x40, 0).unwrap();
    m.step_storage().unwrap();
    assert_eq!(m.irqs_delivered, 0);
    m.board.write8(INTC + 0x40, 3).unwrap();
    m.cpu.sr = 0x2300;
    m.step_storage().unwrap();
    assert_eq!(m.irqs_delivered, 0);
    assert_eq!(pending(&mut m), DMA59);
    m.cpu.sr = 0x2000;
    m.step_storage().unwrap();
    assert_eq!(m.cpu.pc, DMA_HANDLER + 2);
}

#[test]
fn tc_requires_completed_dma_and_major_irq_requires_int_major() {
    let mut m = device();
    m.board
        .write32(
            esdhc::BASE + esdhc::XFERTYP,
            (18 << 24) | (1 << 21) | (1 << 4),
        )
        .unwrap();
    assert_eq!(m.board.read32(esdhc::BASE + esdhc::IRQSTAT).unwrap() & 2, 0);
    read_dma(&mut m, false);
    assert_eq!(m.board.read32(esdhc::BASE + esdhc::IRQSTAT).unwrap() & 2, 2);
    assert_eq!(pending(&mut m), 0);
    // CITER zero does not complete another data transfer.
    m.board.write32(esdhc::BASE + esdhc::IRQSTAT, 3).unwrap();
    m.board
        .write16(edma::TCD_BASE + 59 * 0x20 + edma::CITER as u32, 0)
        .unwrap();
    m.board
        .write32(
            esdhc::BASE + esdhc::XFERTYP,
            (18 << 24) | (1 << 21) | (1 << 4),
        )
        .unwrap();
    assert_eq!(m.board.read32(esdhc::BASE + esdhc::IRQSTAT).unwrap() & 2, 0);
}

#[test]
fn cint_and_w1c_acknowledge_only_selected_latches() {
    let mut m = device();
    // Seed unrelated register latches through the shared backing bank, as
    // another channel model would; guest writes themselves are W1C.
    m.board
        .dma
        .edma_regs
        .write(edma::INTH, 4, DMA59 | (1 << 28) | 1);
    m.board.dma.edma_regs.write(edma::INTL, 4, 3);
    m.board.write8(edma::CINT, 0x80 | 59).unwrap();
    assert_eq!(pending(&mut m), DMA59 | (1 << 28) | 1);
    m.board.write8(edma::CINT, 58).unwrap();
    assert_eq!(pending(&mut m), DMA59 | (1 << 28) | 1);
    m.board.write8(edma::CINT, 59).unwrap();
    assert_eq!(pending(&mut m), (1 << 28) | 1);
    assert_eq!(m.board.read32(edma::INTL).unwrap(), 3);
    m.board.write8(edma::INTH, 0x10).unwrap(); // BE high byte clears channel60
    assert_eq!(pending(&mut m), 1);
    m.board.write16(edma::INTL + 2, 2).unwrap();
    assert_eq!(m.board.read32(edma::INTL).unwrap(), 1);
    let before = pending(&mut m);
    assert!(m.board.write_guest(edma::INTH + 3, 2, 0xffff).is_err());
    assert_eq!(pending(&mut m), before);
    assert_eq!(m.board.read32(edma::INTL).unwrap(), 1);
    m.board.write8(edma::CINT, 0xc0).unwrap(); // NOP takes precedence over CAIR
    assert_eq!(pending(&mut m), 1);
    m.board.write8(edma::CINT, 0x40).unwrap();
    assert_eq!(pending(&mut m), 0);
    assert_eq!(m.board.read32(edma::INTL).unwrap(), 0);
}

#[test]
fn register_pending_survives_existing_board_snapshot() {
    let mut m = device();
    read_dma(&mut m, true);
    drain(&mut m);
    let mut writer = periph::snap::Writer::new();
    m.board.snap_save(&mut writer).unwrap();
    let mut restored = device();
    restored
        .board
        .snap_load(&mut periph::snap::Reader::new(&writer.buf))
        .unwrap();
    assert_eq!(pending(&mut restored), DMA59);
    restored.step_storage().unwrap();
    assert_eq!(restored.cpu.pc, SD_HANDLER + 2);
}

fn reject_unchanged(m: &mut Machine, error: StorageStepError) {
    let regs = (
        m.cpu.pc,
        m.cpu.sr,
        m.cpu.a,
        m.cpu.d,
        m.clock,
        m.irqs_delivered,
    );
    let events = m.board.completion_events().to_vec();
    let inth = pending(m);
    let status = m.board.read32(esdhc::BASE + esdhc::IRQSTAT).unwrap();
    assert_eq!(m.step_storage(), Err(error.clone()));
    assert_eq!(m.step_storage_with_forced(), Err(error.clone()));
    assert_eq!(m.step_device_events(), Err(error));
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
    assert_eq!(pending(m), inth);
    assert_eq!(
        m.board.read32(esdhc::BASE + esdhc::IRQSTAT).unwrap(),
        status
    );
}

#[test]
fn route_preflight_rejects_before_draining_queued_effects() {
    reject_unchanged(
        &mut setup(CompletionPolicy::Oracle, Some(TimerPolicy::Device)),
        StorageStepError::DeviceCompletionRequired,
    );
    reject_unchanged(
        &mut setup(CompletionPolicy::Device, None),
        StorageStepError::TimerNotAttached,
    );
    reject_unchanged(
        &mut setup(CompletionPolicy::Device, Some(TimerPolicy::Oracle)),
        StorageStepError::DeviceTimerRequired,
    );
    let mut m = device();
    read_dma(&mut m, true);
    m.configure_dma_irq(Some((210, 3))).unwrap();
    reject_unchanged(&mut m, StorageStepError::LegacyDmaIrqConfigured);
    m.configure_dma_irq(None).unwrap();
    // Rejection retained both event and external-write queues.
    assert!(!m.board.take_dma_written_ranges().is_empty());
    m.step().unwrap(); // ordinary API creates its legacy pending bool
    reject_unchanged(&mut m, StorageStepError::LegacyDmaIrqPending);
    let mut m = device();
    read_dma(&mut m, true);
    m.board
        .dma
        .edma_regs
        .write(edma::INTH, 4, DMA59 | (1 << 24));
    reject_unchanged(
        &mut m,
        StorageStepError::UnsupportedDmaGroupSources { pending: 1 << 24 },
    );
    assert!(!m.board.take_dma_written_ranges().is_empty());
}

#[test]
fn oracle_interrupt_register_writes_keep_passthrough_behavior() {
    let mut m = setup(CompletionPolicy::Oracle, Some(TimerPolicy::Oracle));
    m.board.write32(edma::INTH, DMA59).unwrap();
    m.board.write8(edma::CINT, 59).unwrap();
    assert_eq!(pending(&mut m), DMA59);
    m.board.write32(edma::INTH, 1).unwrap();
    assert_eq!(pending(&mut m), 1);
    assert_eq!(m.board.read32(INTC + 0x0c).unwrap(), u32::MAX);
}

#[test]
fn queued_dma_invalidates_decoded_code_before_handler_or_normal_instruction() {
    for (deliver, route) in [(false, 0), (true, 0), (false, 1), (true, 1), (false, 2), (true, 2)] {
        let mut m = device();
        let mut payload = [0u8; 512];
        payload[..2].copy_from_slice(&0x7002u16.to_be_bytes()); // MOVEQ #2,D0
        m.board
            .esdhc
            .card_mut()
            .write_data(25, 0, &payload)
            .unwrap();
        m.board.write16(DEST, 0x7001).unwrap(); // MOVEQ #1,D0
        m.cpu.pc = DEST;
        m.cpu.step(&mut m.board).unwrap(); // warm CPU decode cache
        assert_eq!(m.cpu.d[0], 1);
        m.cpu.pc = if deliver { CODE } else { DEST };
        read_dma(&mut m, true);
        m.board.write32(esdhc::BASE + esdhc::IRQSIGEN, 0).unwrap();
        if deliver {
            m.board.write32(RAM + 192 * 4, DEST).unwrap();
        } else {
            m.board.write8(INTC + 0x1c, 0).unwrap();
        }
        match route {
            0 => m.step_storage().unwrap(),
            1 => m.step_storage_with_forced().unwrap(),
            2 => m.step_device_events().unwrap(),
            _ => unreachable!(),
        };
        assert_eq!(m.cpu.d[0], 2);
        assert_eq!(m.irqs_delivered, u64::from(deliver));
    }
}

#[test]
fn guest_dma_completion_does_not_feed_legacy_latch_and_esdhc_rejects_live_dma() {
    let mut m = device();
    read_dma(&mut m, true);
    m.board.write32(esdhc::BASE + esdhc::IRQSIGEN, 0).unwrap();
    m.board.write8(INTC + 0x1c, 0).unwrap();
    m.step_storage().unwrap(); // drains queued events without a legacy latch
    let pc = m.cpu.pc;
    let clock = m.clock;
    assert_eq!(
        m.step_esdhc(),
        Err(machine::runner::EsdhcStepError::LegacyDmaIrqPending)
    );
    assert_eq!((m.cpu.pc, m.clock), (pc, clock));
    // Generate the next DMA completion from a guest instruction through the
    // new route, rather than from a host Board write.
    m.board.write8(edma::CINT, 59).unwrap();
    m.board.write16(CODE + 2, 0x23c0).unwrap(); // MOVE.L D0,(XFERTYP).L
    m.board
        .write32(CODE + 4, esdhc::BASE + esdhc::XFERTYP)
        .unwrap();
    m.cpu.d[0] = (18 << 24) | (1 << 21) | (1 << 4);
    m.step_storage().unwrap();
    assert_eq!(pending(&mut m), DMA59);
    m.board.write8(edma::CINT, 59).unwrap();
    m.step_storage().unwrap(); // must not reject a legacy pending bool
    assert_eq!(m.irqs_delivered, 0);
    m.board.dma.edma_regs.write(edma::INTH, 4, 1 << 24);
    assert_eq!(
        m.step_esdhc(),
        Err(machine::runner::EsdhcStepError::LegacyDmaIrqPending)
    );
}

fn forced_source(m: &mut Machine, bank: usize, source: u32, level: u8) -> u32 {
    let base = intc::BASES[bank];
    let mask = base + if source < 32 { 0x0c } else { 0x08 };
    let old = m.board.read32(mask).unwrap();
    m.board.write32(mask, old & !(1 << (source % 32))).unwrap();
    m.board.write8(base + 0x40 + source, level).unwrap();
    let vector = u32::from(intc::VECTOR_BASE[bank]) + source;
    m.board.write32(RAM + vector * 4, DMA_HANDLER).unwrap();
    base + if source < 32 { 0x14 } else { 0x10 }
}

#[test]
fn storage_isr_guest_requests_force_then_guest_clear_deasserts() {
    let mut m = device();
    let force = forced_source(&mut m, 2, 13, 2);
    m.board.time_mut().unwrap().set_force_irq(false); // Oracle knob is irrelevant
    // Synthetic storage ISR: acknowledge CC, request source 13, then RTE.
    m.cpu.d[0] = 1;
    m.cpu.d[1] = 1 << 13;
    for (off, opcode, addr) in [
        (0, 0x23c0, esdhc::BASE + esdhc::IRQSTAT),
        (6, 0x23c1, force),
    ] {
        m.board.write16(SD_HANDLER + off, opcode).unwrap(); // MOVE.L Dn,(abs).L
        m.board.write32(SD_HANDLER + off + 2, addr).unwrap();
    }
    m.board.write16(SD_HANDLER + 12, 0x4e73).unwrap();
    // Synthetic forced ISR clears the actual force register, then RTE.
    m.board.write16(DMA_HANDLER, 0x23c2).unwrap(); // D2 is zero
    m.board.write32(DMA_HANDLER + 2, force).unwrap();
    m.board.write16(DMA_HANDLER + 6, 0x4e73).unwrap();
    m.board
        .write32(esdhc::BASE + esdhc::XFERTYP, 13 << 24)
        .unwrap();
    m.step_storage_with_forced().unwrap();
    assert_eq!(m.cpu.pc, SD_HANDLER + 6);
    m.step_storage_with_forced().unwrap(); // guest sets INTFRC
    assert_eq!(m.board.read32(force).unwrap(), 1 << 13);
    m.step_storage_with_forced().unwrap(); // RTE; force is below current IPL
    assert_eq!(m.cpu.pc, CODE);
    assert_eq!(m.irqs_delivered, 1);
    m.step_storage_with_forced().unwrap(); // vector 205 entry and guest clear
    assert_eq!(m.cpu.pc, DMA_HANDLER + 6);
    assert_eq!(m.cpu.sr & 0x0700, 0x0200);
    assert_eq!(m.board.read32(force).unwrap(), 0);
    m.step_storage_with_forced().unwrap(); // RTE
    m.step_storage_with_forced().unwrap(); // no recurrence
    assert_eq!(m.cpu.pc, CODE + 2);
    assert_eq!(m.irqs_delivered, 2);
}

#[test]
fn explicit_force_includes_source_zero_and_ssi_and_respects_mask_icr_live_ipl() {
    for (bank, source) in [(0, 0), (1, 63), (2, 63)] {
        let mut m = device();
        let force = forced_source(&mut m, bank, source, 4);
        m.board.write32(force, 1 << (source % 32)).unwrap();
        m.step_storage().unwrap(); // pair-only API ignores forces
        assert_eq!(m.irqs_delivered, 0);
        let mask = intc::BASES[bank] + if source < 32 { 0x0c } else { 0x08 };
        m.board.write32(mask, u32::MAX).unwrap();
        m.step_storage_with_forced().unwrap();
        assert_eq!(m.irqs_delivered, 0);
        m.board.write32(mask, !(1 << (source % 32))).unwrap();
        m.board
            .write8(intc::BASES[bank] + 0x40 + source, 0)
            .unwrap();
        m.step_storage_with_forced().unwrap();
        assert_eq!(m.irqs_delivered, 0);
        m.board
            .write8(intc::BASES[bank] + 0x40 + source, 4)
            .unwrap();
        m.cpu.sr = 0x2400;
        m.step_storage_with_forced().unwrap();
        assert_eq!(m.irqs_delivered, 0);
        m.cpu.sr = 0x2000;
        m.step_storage_with_forced().unwrap();
        assert_eq!(m.cpu.pc, DMA_HANDLER + 2);
        assert_eq!(m.irqs_delivered, 1);
        assert_eq!(m.board.read32(force).unwrap(), 1 << (source % 32));
    }
}

#[test]
fn forced_physical_source_is_one_persistent_request() {
    let mut m = device();
    read_dma(&mut m, true);
    m.board.write32(INTC + 0x14, 1 << 31).unwrap();
    m.step_storage_with_forced().unwrap();
    assert_eq!(m.cpu.pc, SD_HANDLER + 2);
    assert_eq!(m.irqs_delivered, 1);
    m.board.write32(esdhc::BASE + esdhc::IRQSTAT, 3).unwrap();
    m.board.write8(edma::CINT, 59).unwrap();
    m.step_storage_with_forced().unwrap(); // RTE
    m.step_storage_with_forced().unwrap(); // only remaining assertion is force
    assert_eq!(m.cpu.pc, SD_HANDLER + 2);
    assert_eq!(m.irqs_delivered, 2);
    m.board.write32(INTC + 0x14, 0).unwrap();
    m.step_storage_with_forced().unwrap();
    m.step_storage_with_forced().unwrap();
    assert_eq!(m.irqs_delivered, 2);
}

#[test]
fn forced_route_rejects_unmodeled_controller_modes_before_mutation() {
    for (addr, size, value, error) in [
        (
            intc::BASES[0] + 0x1a,
            2,
            1 << 5,
            StorageStepError::UnsupportedControllerLevelMasking,
        ),
        (
            intc::BASES[0] + 0x1e,
            1,
            3,
            StorageStepError::UnsupportedControllerLevelMasking,
        ),
        (INTC + 0x5f, 1, 7, StorageStepError::UnsupportedLevel7),
    ] {
        let mut m = device();
        read_dma(&mut m, true);
        m.board.write_guest(addr, size, value).unwrap();
        let regs = (
            m.cpu.pc,
            m.cpu.sr,
            m.cpu.a,
            m.cpu.d,
            m.clock,
            m.irqs_delivered,
        );
        let events = m.board.completion_events().to_vec();
        assert_eq!(m.step_storage_with_forced(), Err(error));
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
        assert_eq!(pending(&mut m), DMA59);
        assert!(!m.board.take_dma_written_ranges().is_empty());
        m.step_storage().unwrap(); // the existing pair-only contract is unchanged
        assert_eq!(m.irqs_delivered, 1);
    }
}

#[test]
fn forced_entry_failure_is_poisoned() {
    let mut m = device();
    let force = forced_source(&mut m, 0, 0, 3);
    m.board.write32(force, 1).unwrap();
    m.cpu.a[7] = 0;
    assert!(matches!(
        m.step_storage_with_forced(),
        Err(StorageStepError::InterruptDeliveryPoisoned { .. })
    ));
    assert_eq!(m.irqs_delivered, 0);
    assert_eq!(m.clock, 0);
}

fn supplied_timer(m: &mut Machine, pit: bool, channel: usize, level: u8) -> u32 {
    let (bank, source, base) = if pit {
        (2, 13 + channel as u32, periph::pit::BASES[channel])
    } else {
        (0, 32 + channel as u32, periph::dtim::BASES[channel])
    };
    forced_source(m, bank, source, level);
    m.board.write16(base, if pit { 8 } else { 0x001d }).unwrap();
    if pit {
        m.board
            .time_mut()
            .unwrap()
            .device_pit_timeout(channel)
            .unwrap();
    } else {
        m.board
            .time_mut()
            .unwrap()
            .device_dtim_reference(channel)
            .unwrap();
    }
    let handler = RAM + 0x2200 + (u32::from(pit) * 4 + channel as u32) * 0x10;
    let vector = u32::from(intc::VECTOR_BASE[bank]) + source;
    m.board.write32(RAM + vector * 4, handler).unwrap();
    m.board.write16(handler, 0x4e71).unwrap();
    m.board.write16(handler + 2, 0x4e73).unwrap();
    handler
}

#[test]
fn supplied_events_share_level_bank_and_source_priority_with_storage_and_force() {
    for winner in ["level", "bank", "source", "storage"] {
        let mut m = device();
        read_dma(&mut m, true);
        let pit = supplied_timer(&mut m, true, 3, if winner == "level" { 4 } else { 3 });
        let dtim0 = supplied_timer(&mut m, false, 0, 3);
        let dtim3 = supplied_timer(&mut m, false, 3, 3);
        let force = forced_source(&mut m, 1, 20, 3);
        m.board.write32(force, 1 << 20).unwrap();
        if winner == "bank" {
            m.board.write8(periph::dtim::BASES[3] + 3, 2).unwrap();
        }
        if winner == "storage" {
            for channel in [0, 3] {
                m.board.write8(periph::dtim::BASES[channel] + 3, 2).unwrap();
            }
            m.board.write32(force, 0).unwrap();
        }
        let handler = match winner {
            "level" => pit,
            "bank" => dtim0,
            "source" => dtim3,
            "storage" => SD_HANDLER, // source31 outranks PIT3 source16 and DMA0
            _ => unreachable!(),
        };
        m.step_device_events().unwrap();
        assert_eq!(m.cpu.pc, handler + 2, "{winner}");
        assert_eq!(m.irqs_delivered, 1); // only one selection at this boundary
        assert_eq!(m.clock, 1);
        assert_eq!(m.board.read16(periph::pit::BASES[3]).unwrap() & 4, 4);
        assert_eq!(pending(&mut m), DMA59);
    }
}

#[test]
fn supplied_timer_mask_icr_and_ipl_defer_then_status_reasserts_until_ack() {
    for pit in [false, true] {
        let mut m = device();
        let handler = supplied_timer(&mut m, pit, 0, 3);
        let (bank, source) = if pit { (2, 13) } else { (0, 32) };
        let mask = intc::BASES[bank] + if source < 32 { 0x0c } else { 8 };
        let icr = intc::BASES[bank] + 0x40 + source;
        m.board.write32(mask, u32::MAX).unwrap();
        m.step_device_events().unwrap();
        assert_eq!(m.irqs_delivered, 0);
        m.board.write32(mask, !(1 << (source % 32))).unwrap();
        m.board.write8(icr, 0).unwrap();
        m.step_device_events().unwrap();
        assert_eq!(m.irqs_delivered, 0);
        m.board.write8(icr, 3).unwrap();
        m.cpu.sr = 0x2300;
        m.step_device_events().unwrap();
        assert_eq!(m.irqs_delivered, 0);
        m.cpu.sr = 0x2000;
        m.step_device_events().unwrap();
        assert_eq!(m.cpu.pc, handler + 2);
        m.step_device_events().unwrap(); // RTE, same-level request blocked at entry
        m.step_device_events().unwrap(); // status still asserted
        assert_eq!(m.cpu.pc, handler + 2);
        assert_eq!(m.irqs_delivered, 2);
        if pit {
            m.board.write16(periph::pit::BASES[0], 12).unwrap();
        } else {
            m.board.write8(periph::dtim::BASES[0] + 3, 2).unwrap();
        }
        m.step_device_events().unwrap(); // RTE
        m.clock = 1_000_000; // elapsed instructions do not synthesize timer events
        let resume = m.cpu.pc;
        m.step_device_events().unwrap();
        assert_eq!(m.cpu.pc, resume + 2);
        assert_eq!(m.irqs_delivered, 2);
        assert!(
            m.board
                .time()
                .unwrap()
                .device_pit_requests()
                .unwrap()
                .is_empty()
        );
        assert!(
            m.board
                .time()
                .unwrap()
                .device_dtim_reference_requests()
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn existing_pair_and_forced_routes_exclude_supplied_physical_timers() {
    let mut m = device();
    supplied_timer(&mut m, true, 0, 3);
    let dtim = supplied_timer(&mut m, false, 0, 3);
    m.step_storage().unwrap();
    m.step_storage_with_forced().unwrap();
    assert_eq!(m.irqs_delivered, 0);
    m.step_device_events().unwrap();
    assert_eq!(m.cpu.pc, dtim + 2);
    assert_eq!(m.irqs_delivered, 1);
}

#[test]
fn physical_pit_and_software_force_alias_one_entry_until_both_acknowledged() {
    for clear_force_first in [false, true] {
        let mut m = device();
        let handler = supplied_timer(&mut m, true, 0, 3);
        let force = INTC + 0x14;
        m.board.write32(force, 1 << 13).unwrap();
        m.step_device_events().unwrap();
        assert_eq!(m.cpu.pc, handler + 2);
        assert_eq!(m.irqs_delivered, 1);
        if clear_force_first {
            m.board.write32(force, 0).unwrap();
        } else {
            m.board.write16(periph::pit::BASES[0], 12).unwrap();
        }
        m.step_device_events().unwrap(); // RTE
        m.step_device_events().unwrap();
        assert_eq!(m.cpu.pc, handler + 2);
        assert_eq!(m.irqs_delivered, 2);
        if clear_force_first {
            m.board.write16(periph::pit::BASES[0], 12).unwrap();
        } else {
            m.board.write32(force, 0).unwrap();
        }
        m.step_device_events().unwrap(); // RTE
        m.step_device_events().unwrap();
        assert_eq!(m.irqs_delivered, 2);
    }
}

#[test]
fn supplied_event_preflight_rejects_modes_before_cpu_events_or_cache_effects() {
    for rejection in ["dtim", "level7", "emask", "clmask"] {
        let mut m = device();
        m.board.write16(DEST, 0x7001).unwrap();
        m.cpu.pc = DEST;
        m.cpu.step(&mut m.board).unwrap();
        assert!(m.cpu.decoded_at(DEST));
        read_dma(&mut m, true); // queue external write invalidation and completion
        supplied_timer(&mut m, true, 0, if rejection == "level7" { 7 } else { 3 });
        let error = match rejection {
            "dtim" => {
                let mut page = [0; 16];
                page[2] = 0x20; // unsupported EN1588 loaded by a checkpoint
                m.board
                    .time_mut()
                    .unwrap()
                    .load_page(periph::dtim::BASES[3], &page);
                StorageStepError::DeviceTimerRequests(machine::DeviceDtimError::UnsupportedMode {
                    channel: 3,
                })
            }
            "level7" => StorageStepError::UnsupportedLevel7,
            "emask" => {
                m.board.write16(intc::BASES[0] + 0x1a, 1 << 5).unwrap();
                StorageStepError::UnsupportedControllerLevelMasking
            }
            "clmask" => {
                m.board.write8(intc::BASES[0] + 0x1e, 3).unwrap();
                StorageStepError::UnsupportedControllerLevelMasking
            }
            _ => unreachable!(),
        };
        let before = (
            m.cpu.pc,
            m.cpu.sr,
            m.cpu.a,
            m.cpu.d,
            m.clock,
            m.irqs_delivered,
        );
        let events = m.board.completion_events().to_vec();
        assert_eq!(m.step_device_events(), Err(error));
        assert_eq!(
            (
                m.cpu.pc,
                m.cpu.sr,
                m.cpu.a,
                m.cpu.d,
                m.clock,
                m.irqs_delivered
            ),
            before
        );
        assert_eq!(m.board.completion_events(), events);
        assert_eq!(pending(&mut m), DMA59);
        assert!(m.cpu.decoded_at(DEST));
        assert!(!m.board.take_dma_written_ranges().is_empty());
        assert_eq!(m.board.take_dma_sources(), ["esdhc-dma59"]);
    }
}

fn supplied_ssi50(m: &mut Machine, level: u8) -> u32 {
    let base = edma::TCD_BASE + (periph::ssi::TX_CHAN * 0x20) as u32;
    m.board.write16(base + edma::ATTR as u32, 0x0202).unwrap();
    m.board.write32(base + edma::NBYTES as u32, 4).unwrap();
    m.board
        .write32(base + edma::SADDR as u32, DEST + 0x200)
        .unwrap();
    m.board.write16(base + edma::SOFF as u32, 4).unwrap();
    m.board.write16(base + edma::CITER as u32, 1).unwrap();
    m.board.write16(base + edma::BITER as u32, 1).unwrap();
    m.board
        .write16(base + edma::CSR as u32, edma::CSR_INT_MAJOR)
        .unwrap();
    m.board
        .enable_ssi_diagnostic(periph::ssi::AUDIO_SSI0_REQUEST_HZ, 132_000_000);
    m.board.write8(edma::SERQ, 50).unwrap();
    let due = m.board.ssi_deadline(0).unwrap();
    m.board.service_ssi(due);
    assert!(m.board.device_ssi50_pending());
    forced_source(m, 1, 42, level);
    let handler = RAM + 0x2300;
    m.board.write32(RAM + 170 * 4, handler).unwrap();
    m.board.write16(handler, 0x4e71).unwrap();
    m.board.write16(handler + 2, 0x4e73).unwrap();
    handler
}

#[test]
fn shared_selection_orders_ssi50_with_timers_storage_and_force191() {
    for winner in ["level", "bank", "source", "physical"] {
        let mut m = device();
        read_dma(&mut m, true);
        supplied_timer(&mut m, true, 0, 3);
        let dtim = supplied_timer(&mut m, false, 0, 3);
        let ssi = supplied_ssi50(&mut m, if winner == "level" { 4 } else { 3 });
        let force = forced_source(&mut m, 1, 63, 3);
        m.board.write32(force, 1 << 31).unwrap();
        if matches!(winner, "source" | "physical") {
            m.board.write8(periph::dtim::BASES[0] + 3, 2).unwrap();
        }
        if winner == "physical" {
            m.board.write32(force, 0).unwrap();
        }
        let handler = match winner {
            "level" | "physical" => ssi,
            "bank" => dtim,
            "source" => DMA_HANDLER, // force191 outranks SSI170 within INTC1
            _ => unreachable!(),
        };
        m.step_device_events().unwrap();
        assert_eq!(m.cpu.pc, handler + 2, "{winner}");
        assert_eq!(m.irqs_delivered, 1);
        assert!(m.board.device_ssi50_pending());
    }
}

#[test]
fn ssi50_persists_across_entry_and_rte_and_force191_acknowledges_independently() {
    let mut m = device();
    let ssi = supplied_ssi50(&mut m, 3);
    m.step_storage().unwrap();
    m.step_storage_with_forced().unwrap();
    assert_eq!(m.irqs_delivered, 0);
    // Oracle diagnostic delivery suppression must not hide the Device latch.
    m.board.dma.ssi.as_mut().unwrap().mark_vector170_delivered();
    m.step_device_events().unwrap();
    assert_eq!(m.cpu.pc, ssi + 2);
    m.step_device_events().unwrap(); // RTE
    m.step_device_events().unwrap();
    assert_eq!(m.cpu.pc, ssi + 2);
    assert_eq!(m.irqs_delivered, 2);
    let force = forced_source(&mut m, 1, 63, 3);
    m.board.write32(force, 1 << 31).unwrap();
    m.board.write8(edma::CINT, 50).unwrap();
    assert!(!m.board.device_ssi50_pending());
    assert!(m.board.dma.ssi.as_ref().unwrap().vector191_owed());
    m.step_device_events().unwrap(); // RTE from SSI
    m.step_device_events().unwrap();
    assert_eq!(m.cpu.pc, DMA_HANDLER + 2);
    assert_eq!(m.irqs_delivered, 3);
    m.board.write32(force, 0).unwrap();
    m.step_device_events().unwrap(); // RTE
    let resume = m.cpu.pc;
    m.step_device_events().unwrap();
    assert_eq!(m.cpu.pc, resume + 2);
    assert_eq!(m.irqs_delivered, 3);
    assert!(!m.board.device_ssi50_pending());
}

#[test]
fn device_ssi_rx_ranges_invalidate_cached_code_before_shared_entry_or_execution() {
    for (deliver, shared) in [(false, false), (false, true), (true, true)] {
        let mut m = device();
        m.board.write16(DEST, 0x7001).unwrap();
        m.cpu.pc = DEST;
        m.cpu.step(&mut m.board).unwrap();
        assert!(m.cpu.decoded_at(DEST));
        m.cpu.pc = CODE;
        let ssi_handler = supplied_ssi50(&mut m, 3);
        let base = edma::TCD_BASE + (periph::ssi::RX_CHAN * 0x20) as u32;
        m.board.write16(base + edma::ATTR as u32, 0x0202).unwrap();
        m.board.write32(base + edma::NBYTES as u32, 4).unwrap();
        m.board.write32(base + edma::DADDR as u32, DEST).unwrap();
        m.board.write16(base + edma::DOFF as u32, 4).unwrap();
        m.board.write16(base + edma::CITER as u32, 1).unwrap();
        m.board.write16(base + edma::BITER as u32, 1).unwrap();
        m.board.write8(edma::SERQ, 48).unwrap();
        let due = m.board.ssi_deadline(0).unwrap();
        assert_eq!(m.board.service_ssi(due), vec![(DEST, 4)]);
        assert_eq!(
            m.board.read32(DEST).unwrap(),
            periph::ssi::RxHandoverPeer::MARKER
        );
        assert!(m.cpu.decoded_at(DEST)); // board writes alone cannot evict CPU decode
        if !deliver {
            m.board.write32(intc::BASES[1] + 8, u32::MAX).unwrap();
        }
        if shared {
            m.step_device_events().unwrap();
        } else {
            m.step().unwrap();
        }
        assert!(!m.cpu.decoded_at(DEST));
        assert!(m.board.take_dma_written_ranges().is_empty());
        assert_eq!(m.cpu.pc, if deliver { ssi_handler + 2 } else { CODE + 2 });
        assert_eq!(m.irqs_delivered, u64::from(deliver));
        assert!(m.board.take_dma_sources().is_empty());
        for _ in 0..3 {
            let due = m.board.ssi_deadline(due).unwrap();
            assert_eq!(m.board.service_ssi(due).len(), 1);
            if shared {
                m.step_device_events().unwrap();
            } else {
                m.step().unwrap();
            }
            assert!(m.board.take_dma_written_ranges().is_empty());
            assert!(m.board.take_dma_sources().is_empty());
        }
    }
}
