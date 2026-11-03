//! Opt-in stock ISR proofs: the original DT2 replay uses a controlled W1C
//! double; the DT2/DN2 delivery fixture uses real Board eSDHC/INTC registers and
//! the actual signal helper with one waiter. Queue insertion and the scheduling
//! request are observed in the earlier delivery proofs. The pair fixture proves
//! a controlled guest-triggered DMA59 copy before both actual ISRs; injected
//! error bits do not model corrupt DMA. A separate scheduler fixture follows
//! the real forced request into one initialized task and its guest marker.
//! None of these fixtures establishes native-boot completion or full scheduling.
use coldfire::{Bus, BusError, Cpu, InterruptPolicy};
use patchkit::build::{cf, toolchain::Env};
use patchkit::error::sha256_hex;
use patchkit::firmware::Firmware;
use patchkit::link::Linked;
use std::path::Path;

const STOCK_SHA: &str = "26c22f6652625ac2cfd47f7ee970d388ed8b6427dae3563c0a6a2f2d334350d5";
const SITE_SHA: &str = "ecda53f8ded4ab291974386fad6a07fc3d8672509f611233aff4a9c2560b15f9";
const SITES: [u32; 3] = [0x4012_d5c2, 0x4012_d616, 0x4012_d642];
const ISR: u32 = 0x4012_d5ba;
const IRQSTAT: u32 = 0xfc0c_c030;
const MASK: u32 = 0x0031_0000;
const STATUS: [u32; 2] = [0x44e3_fea8, 0x44e3_feac];
const SEM: [u32; 2] = [0x44e3_fec0, 0x44e3_fec8];
const LOAD: u32 = 0x1000;
const SP: u32 = 0xf000;
const RETURN: u32 = 0x9000;
const VBR: u32 = 0x8000;
const VECTOR: u8 = 0x80;
const CEILING: usize = 2048;

fn compiled() -> Linked {
    compiled_at(SITES)
}

fn compiled_at(sites: [u32; 3]) -> Linked {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let env = Env::new(&root);
    env.m68k("as")
        .expect("ISR replay requires the ColdFire assembler");
    let object = cf::cf_object(
        &env,
        &[root.join("modules/core/adapters/storage-fault-proof/src/cf/storage-fault.s")],
        &[],
    )
    .unwrap();
    let externs = ['a', 'b', 'c']
        .into_iter()
        .zip(sites)
        .map(|(name, site)| (format!("storage_fault_site_{name}"), u64::from(site)))
        .collect();
    let linked = patchkit::link::link(&object, u64::from(LOAD), &externs).unwrap();
    assert_eq!(linked.bss_size, 4);
    linked
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Mmio {
    Read(u32),
    Write(u32),
}

#[derive(Clone)]
struct Memory {
    regions: Vec<(u32, Vec<u8>)>,
    irq: u32,
    mmio: Vec<Mmio>,
    writes: Vec<(u32, Vec<u8>)>,
    observer: bool,
    observer_sp: u32,
    latch: u32,
}
impl Memory {
    fn new(fw: &Firmware, linked: &Linked, patched: bool) -> Self {
        let main = fw.section(3).expect("stock MAIN section");
        let mut memory = Self {
            regions: vec![
                (main.dest, main.data.clone()),
                (0, vec![0xa5; 0x10000]),
                (0x44e3_fe00, vec![0; 0x100]),
            ],
            irq: 0,
            mmio: vec![],
            writes: vec![],
            observer: false,
            observer_sp: 0,
            latch: linked.symbols["storage_fault_bits"] as u32,
        };
        memory.put(LOAD, &linked.image).unwrap();
        memory.put(memory.latch, &[0; 4]).unwrap();
        memory
            .put(VBR + u32::from(VECTOR) * 4, &ISR.to_be_bytes())
            .unwrap();
        for (site, name) in SITES.into_iter().zip(['a', 'b', 'c']) {
            assert_eq!(
                sha256_hex(memory.get(site, 6).unwrap()),
                SITE_SHA,
                "original IRQSTAT load at {site:#x}"
            );
            if patched {
                let target = linked.symbols[&format!("storage_fault_hook_{name}")] as u32;
                let mut jump = vec![0x4e, 0xf9];
                jump.extend_from_slice(&target.to_be_bytes());
                memory.put(site, &jump).unwrap();
            }
        }
        memory.writes.clear();
        memory
    }
    fn get(&self, at: u32, n: usize) -> Result<&[u8], BusError> {
        self.regions
            .iter()
            .find_map(|(base, data)| {
                let offset = at.checked_sub(*base)? as usize;
                data.get(offset..offset.checked_add(n)?)
            })
            .ok_or(BusError {
                addr: at,
                write: false,
            })
    }
    fn put(&mut self, at: u32, bytes: &[u8]) -> Result<(), BusError> {
        for (base, data) in &mut self.regions {
            if let Some(offset) = at.checked_sub(*base).map(|v| v as usize)
                && let Some(dest) = data.get_mut(offset..offset + bytes.len())
            {
                dest.copy_from_slice(bytes);
                return Ok(());
            }
        }
        Err(BusError {
            addr: at,
            write: true,
        })
    }
    fn write(&mut self, at: u32, bytes: &[u8]) -> Result<(), BusError> {
        if self.observer {
            assert!(
                at == self.latch && bytes.len() == 4
                    || (self.observer_sp - 8..self.observer_sp - 2).contains(&at)
                        && at + bytes.len() as u32 <= self.observer_sp - 2,
                "observer wrote outside its latch/temporary stack: {at:#x}"
            );
        } else {
            self.writes.push((at, bytes.to_vec()));
        }
        self.put(at, bytes)
    }
    fn word(&self, at: u32) -> u32 {
        u32::from_be_bytes(self.get(at, 4).unwrap().try_into().unwrap())
    }
}
impl Bus for Memory {
    fn read8(&mut self, at: u32) -> Result<u8, BusError> {
        Ok(self.get(at, 1)?[0])
    }
    fn read16(&mut self, at: u32) -> Result<u16, BusError> {
        Ok(u16::from_be_bytes(self.get(at, 2)?.try_into().unwrap()))
    }
    fn read32(&mut self, at: u32) -> Result<u32, BusError> {
        if at == IRQSTAT {
            self.mmio.push(Mmio::Read(self.irq));
            return Ok(self.irq);
        }
        Ok(u32::from_be_bytes(self.get(at, 4)?.try_into().unwrap()))
    }
    fn write8(&mut self, at: u32, value: u8) -> Result<(), BusError> {
        self.write(at, &[value])
    }
    fn write16(&mut self, at: u32, value: u16) -> Result<(), BusError> {
        self.write(at, &value.to_be_bytes())
    }
    fn write32(&mut self, at: u32, value: u32) -> Result<(), BusError> {
        if at == IRQSTAT {
            self.mmio.push(Mmio::Write(value));
            self.irq &= !value;
            return Ok(());
        }
        self.write(at, &value.to_be_bytes())
    }
}

fn context(sr: u16) -> Cpu {
    let mut cpu = Cpu::new();
    cpu.pc = RETURN;
    cpu.sr = sr;
    cpu.ctrl.vbr = VBR;
    for i in 0..8 {
        cpu.d[i] = 0x8765_0010 + i as u32 * 0x101;
        cpu.a[i] = 0x1234_1000 + i as u32 * 0x101;
    }
    cpu.a[7] = SP;
    cpu
}

fn replay(memory: &mut Memory, linked: &Linked, irq: u32, sr: u16) -> Cpu {
    memory.irq = irq;
    memory.mmio.clear();
    memory.writes.clear();
    let mut cpu = context(sr);
    let before_d = cpu.d;
    let before_a = cpu.a;
    // Synthetic vector/level are controlled fixture setup, not the device's
    // interrupt installation or arbitration. Exception entry and RTE are real.
    assert!(
        cpu.take_interrupt(memory, VECTOR, Some(3), InterruptPolicy::Device)
            .unwrap()
    );
    let mut signal_entries = [0usize; 2];
    let mut rte_entries = 0;
    for _ in 0..CEILING {
        if cpu.pc == RETURN {
            cpu.resolve_nzv();
            assert_eq!(cpu.d, before_d);
            assert_eq!(cpu.a, before_a);
            assert_eq!(cpu.sr, sr);
            assert_eq!(rte_entries, 1, "return through actual ISR RTE");
            let signals = (irq & 0x0031_0003).count_ones() as usize;
            assert_eq!(signal_entries, [signals; 2], "real signal helper path");
            return cpu;
        }
        if cpu.pc == 0x4012_d69e {
            rte_entries += 1;
        }
        if cpu.pc == 0x4000_155c {
            signal_entries[0] += 1;
        }
        if cpu.pc == 0x4000_11ee {
            signal_entries[1] += 1;
        }
        memory.observer = (LOAD..LOAD + linked.image.len() as u32).contains(&cpu.pc);
        if ['a', 'b', 'c']
            .into_iter()
            .any(|name| linked.symbols[&format!("storage_fault_hook_{name}")] as u32 == cpu.pc)
        {
            memory.observer_sp = cpu.a[7];
        }
        let pc = cpu.pc;
        cpu.step(memory)
            .unwrap_or_else(|error| panic!("ISR step at {pc:#x}: {error:?}"));
        assert_eq!(
            cpu.last_exception, None,
            "unexpected exception from {pc:#x}"
        );
    }
    panic!("ISR exceeded {CEILING} instructions at {:#x}", cpu.pc);
}

fn expected_mmio(mut irq: u32) -> Vec<Mmio> {
    let mut events = vec![];
    for bit in [0x10000, 2, 0x200000, 0x100000, 1] {
        events.push(Mmio::Read(irq));
        if irq & bit != 0 {
            events.push(Mmio::Write(bit));
            irq &= !bit;
        }
    }
    events
}

#[test]
fn actual_isr_preserves_stock_results_and_latches_errors() {
    let Some(path) = std::env::var_os("DIGI_STORAGE_FAULT_ISR_SYX") else {
        eprintln!(
            "SKIP controlled storage ISR replay: set DIGI_STORAGE_FAULT_ISR_SYX to stock DT2 1.17 .syx"
        );
        return;
    };
    let raw =
        std::fs::read(&path).expect("DIGI_STORAGE_FAULT_ISR_SYX must name a readable stock image");
    let fw = Firmware::parse(&raw).expect("parse configured stock image");
    assert_eq!(fw.syx_sha256, STOCK_SHA, "requires exact stock DT2 1.17");
    let linked = compiled();
    let cases: &[&[u32]] = &[
        &[0],
        &[0x10000],
        &[0x100000],
        &[0x200000],
        &[1],
        &[2],
        &[3],
        &[0x10001],
        &[0x100001],
        &[0x200001],
        &[0x310003],
        &[0x10000, 0, 0x100001, 0x200002, 1],
    ];
    for case in cases {
        for sr in [0x2000, 0x201f, 0x2215] {
            let mut stock = Memory::new(&fw, &linked, false);
            let mut patched = Memory::new(&fw, &linked, true);
            let mut sticky = 0;
            for &irq in *case {
                let original = replay(&mut stock, &linked, irq, sr);
                let observed = replay(&mut patched, &linked, irq, sr);
                assert_eq!(
                    (
                        observed.d,
                        observed.a,
                        observed.sr,
                        observed.pc,
                        observed.other_a7
                    ),
                    (
                        original.d,
                        original.a,
                        original.sr,
                        original.pc,
                        original.other_a7
                    )
                );
                assert_eq!(stock.mmio, expected_mmio(irq));
                assert_eq!(patched.mmio, stock.mmio);
                assert_eq!(patched.irq, stock.irq);
                assert_eq!(
                    patched.writes, stock.writes,
                    "all writes excluding observer latch/temporary stack"
                );
                for at in STATUS.into_iter().chain(SEM).chain(SEM.map(|at| at + 4)) {
                    assert_eq!(patched.word(at), stock.word(at), "status/signal {at:#x}");
                }
                sticky |= irq & MASK;
                assert_eq!(patched.word(patched.latch), sticky);
                assert_eq!(stock.word(stock.latch), 0);
            }
        }
    }
}

// Controlled initialized-state delivery through Board's real eSDHC registers,
// shared INTC bank and Machine::step_esdhc. Queue insertion proves a waiter
// becomes ready; the forced scheduling request is observed but never serviced.
struct DeliveryTarget {
    name: &'static str,
    env: &'static str,
    sha: &'static str,
    sites: [u32; 3],
    isr: u32,
    rte: u32,
    status: [u32; 2], // data, command
    sem: [u32; 2],
    high_ready: u32,
    dma_isr: u32,
    dma_rte: u32,
    dma_sem: u32,
}

const DELIVERY_TARGETS: [DeliveryTarget; 2] = [
    DeliveryTarget {
        name: "DT2 1.17",
        env: "DIGI_STORAGE_FAULT_ISR_SYX",
        sha: STOCK_SHA,
        sites: SITES,
        isr: ISR,
        rte: 0x4012_d69e,
        status: STATUS,
        sem: SEM,
        high_ready: 0x4096_4914,
        dma_isr: 0x4012_d6a0,
        dma_rte: 0x4012_d6c6,
        dma_sem: 0x44e3_feb8,
    },
    DeliveryTarget {
        name: "DN2 1.12",
        env: "DIGI_STORAGE_FAULT_ISR_DN2_SYX",
        sha: "7e5b8a733d4ab1fdf7e5d22757f7d8eddaa3cbf71d0858874a791844c02ceb35",
        sites: [0x4012_bc86, 0x4012_bcda, 0x4012_bd06],
        isr: 0x4012_bc7e,
        rte: 0x4012_bd62,
        status: [0x4461_c6b8, 0x4461_c6bc],
        sem: [0x4461_c6d0, 0x4461_c6d8],
        high_ready: 0x4058_e15c,
        dma_isr: 0x4012_bd64,
        dma_rte: 0x4012_bd8a,
        dma_sem: 0x4461_c6c8,
    },
];
const WAITER: u32 = 0x4100_0100;
const READY_HEAD: u32 = 0x4100_0200;
const INTC2: u32 = 0xfc05_0000;
const DELIVERY_VECTOR: u32 = 223;

#[derive(Debug, PartialEq, Eq)]
struct DeliveryResult {
    d: [u32; 8],
    a: [u32; 8],
    sr: u16,
    pc: u32,
    other_a7: u32,
    status: [u32; 2],
    sem: [u32; 4],
    queue: [u32; 5],
    forced: u32,
    irqstat: u32,
    signal_entries: [usize; 2],
}

fn import_delivery_main(board: &mut machine::Board, base: u32, data: &[u8]) {
    let page_size = machine::state::PAGE_SIZE;
    let first = base & !(page_size as u32 - 1);
    let end = u64::from(base) + data.len() as u64;
    for page in (u64::from(first)..end).step_by(page_size) {
        let start = page.max(u64::from(base));
        let stop = (page + page_size as u64).min(end);
        let mut bytes = vec![0; page_size];
        bytes[(start - page) as usize..(stop - page) as usize].copy_from_slice(
            &data[(start - u64::from(base)) as usize..(stop - u64::from(base)) as usize],
        );
        board.import_ram_page(page as u32, &bytes).unwrap();
    }
}

fn delivery_machine(
    fw: &Firmware,
    linked: &Linked,
    target: &DeliveryTarget,
    patched: bool,
    waiter_sem: usize,
) -> machine::Machine {
    use machine::{Board, CompletionPolicy, Machine, SemaphoreAddresses, Time, TimerPolicy};
    // Populate real firmware MAIN only; all other memory is controlled fixture
    // RAM, not a restored boot or runtime state.
    let main = fw.section(3).expect("stock MAIN section");
    let mut board = Board::new(
        Default::default(),
        SemaphoreAddresses {
            dma_sem: None,
            data_sem: Some(target.sem[0]),
            cmd_sem: Some(target.sem[1]),
            status: None,
        },
        CompletionPolicy::Device,
    );
    import_delivery_main(&mut board, main.dest, &main.data);
    for address in [0, WAITER, target.status[0], target.high_ready] {
        board.map_ram_page(address & !0x000f_ffff).unwrap();
    }
    for (offset, byte) in linked.image.iter().enumerate() {
        board.write8(LOAD + offset as u32, *byte).unwrap();
    }
    board
        .write32(linked.symbols["storage_fault_bits"] as u32, 0)
        .unwrap();
    for (site, name) in target.sites.into_iter().zip(['a', 'b', 'c']) {
        let offset = (site - main.dest) as usize;
        assert_eq!(
            sha256_hex(&main.data[offset..offset + 6]),
            SITE_SHA,
            "{} original IRQSTAT load {site:#x}",
            target.name
        );
        if patched {
            board.write16(site, 0x4ef9).unwrap();
            board
                .write32(
                    site + 2,
                    linked.symbols[&format!("storage_fault_hook_{name}")] as u32,
                )
                .unwrap();
        }
    }
    board
        .write32(VBR + DELIVERY_VECTOR * 4, target.isr)
        .unwrap();
    // MOVE.L D0,(XFERTYP).L creates CC as an ordinary guest instruction.
    board.write16(RETURN, 0x23c0).unwrap();
    board.write32(RETURN + 2, 0xfc0c_c00c).unwrap();
    board.write16(RETURN + 6, 0x4e71).unwrap();
    for address in target
        .status
        .into_iter()
        .chain(target.sem)
        .chain(target.sem.map(|at| at + 4))
        .chain([target.high_ready, READY_HEAD])
    {
        board.write32(address, 0).unwrap();
    }
    board.write32(target.sem[waiter_sem] + 4, WAITER).unwrap();
    board.write32(WAITER, 0).unwrap();
    board.write32(WAITER + 4, 0).unwrap();
    board.write32(WAITER + 8, READY_HEAD).unwrap();
    board.attach_time(Time::new(TimerPolicy::Device, vec![], 1_000_000.0));
    board.write32(INTC2 + 0x0c, u32::MAX).unwrap();
    board.write8(INTC2 + 0x5f, 3).unwrap();
    board.write8(INTC2 + 0x1d, 31).unwrap();
    board.write32(0xfc0c_c034, 0x0031_0003).unwrap(); // explicit IRQSTATEN
    board.write32(0xfc0c_c038, 0x0031_0003).unwrap(); // explicit IRQSIGEN
    let mut cpu = context(0x2215);
    cpu.d[0] = 13 << 24; // CMD13, no DMA
    Machine::new(cpu, board)
}

fn delivered_replay(
    fw: &Firmware,
    linked: &Linked,
    target: &DeliveryTarget,
    patched: bool,
    extra_irq: u32,
    waiter_sem: usize,
) -> DeliveryResult {
    let mut m = delivery_machine(fw, linked, target, patched, waiter_sem);
    assert!(
        m.step_esdhc()
            .unwrap()
            .iter()
            .all(|event| !matches!(event, machine::CompletionEvent::Dma59 { .. }))
    );
    // CC is already latched, but no ISR has run and Device completion has not
    // posted the host semaphore or released the initialized blocked waiter.
    assert_eq!(m.irqs_delivered, 0);
    assert_eq!(m.board.read32(IRQSTAT).unwrap(), 1);
    assert_eq!(m.board.read32(target.sem[waiter_sem]).unwrap(), 0);
    assert_eq!(m.board.read32(target.sem[waiter_sem] + 4).unwrap(), WAITER);
    assert_eq!(m.board.read32(READY_HEAD).unwrap(), 0);
    assert_eq!(m.board.read32(target.high_ready).unwrap(), 0);
    if extra_irq != 0 {
        m.board.esdhc.device_event(extra_irq).unwrap();
    }
    let irq = 1 | extra_irq;
    assert_eq!(m.board.read32(IRQSTAT).unwrap(), irq);
    m.cpu.resolve_nzv();
    let before = (m.cpu.d, m.cpu.a, m.cpu.sr, m.cpu.other_a7);
    let mut signal_entries = [0; 2];
    let mut rte_entries = 0;
    let mut returned = false;
    for _ in 0..CEILING {
        if m.irqs_delivered == 1 && m.cpu.pc == RETURN + 6 {
            returned = true;
            break;
        }
        if m.cpu.pc == target.rte {
            rte_entries += 1;
        }
        if m.cpu.pc == 0x4000_155c {
            signal_entries[0] += 1;
        }
        if m.cpu.pc == 0x4000_11ee {
            signal_entries[1] += 1;
        }
        let pc = m.cpu.pc;
        m.step_esdhc().unwrap_or_else(|error| {
            panic!(
                "{} patched={patched} IRQ={irq:#x} at {pc:#x}: {error:?}",
                target.name
            )
        });
    }
    assert!(
        returned,
        "{} ISR exceeded {CEILING} instructions at {:#x}",
        target.name, m.cpu.pc
    );
    m.cpu.resolve_nzv();
    assert_eq!(
        (m.cpu.d, m.cpu.a, m.cpu.sr, m.cpu.other_a7),
        before,
        "{} preserves interrupted context",
        target.name
    );
    assert_eq!(m.irqs_delivered, 1);
    assert_eq!(rte_entries, 1, "actual ISR RTE");
    assert_eq!(
        signal_entries,
        [irq.count_ones() as usize; 2],
        "actual signal wrapper and helper"
    );
    assert_eq!(
        m.board.read32(IRQSTAT).unwrap(),
        0,
        "actual ISR W1C acknowledgement"
    );
    assert!(!m.board.esdhc.interrupt_pending());
    assert_eq!(m.board.read32(target.sem[waiter_sem] + 4).unwrap(), 0);
    let sem_signals = if waiter_sem == 1 {
        (irq & 0x0001_0001).count_ones()
    } else {
        (irq & 0x0030_0002).count_ones()
    };
    assert_eq!(
        m.board.read32(target.sem[waiter_sem]).unwrap(),
        sem_signals - 1,
        "waiter consumes first signal"
    );
    assert_eq!(m.board.read32(READY_HEAD).unwrap(), WAITER);
    assert_eq!(m.board.read32(WAITER).unwrap(), WAITER);
    assert_eq!(m.board.read32(WAITER + 4).unwrap(), WAITER);
    assert_eq!(m.board.read32(target.high_ready).unwrap(), READY_HEAD);
    assert_eq!(
        m.board.read32(INTC2 + 0x14).unwrap() & (1 << 13),
        1 << 13,
        "ready waiter requests scheduling; request is not serviced"
    );
    assert_eq!(
        m.board.read32(target.status[1]).unwrap(),
        0,
        "CC clears command status after any CTOE"
    );
    assert_eq!(
        m.board.read32(target.status[0]).unwrap(),
        if irq & 0x0030_0000 != 0 { u32::MAX } else { 0 },
        "data error follows TC in the actual ISR"
    );
    let other_sem = 1 - waiter_sem;
    assert_eq!(m.board.read32(target.sem[other_sem] + 4).unwrap(), 0);
    assert_eq!(
        m.board.read32(target.sem[other_sem]).unwrap(),
        if other_sem == 1 {
            (irq & 0x0001_0001).count_ones()
        } else {
            (irq & 0x0030_0002).count_ones()
        }
    );
    let latch = m
        .board
        .read32(linked.symbols["storage_fault_bits"] as u32)
        .unwrap();
    assert_eq!(latch, if patched { irq & MASK } else { 0 });
    DeliveryResult {
        d: m.cpu.d,
        a: m.cpu.a,
        sr: m.cpu.sr,
        pc: m.cpu.pc,
        other_a7: m.cpu.other_a7,
        status: target
            .status
            .map(|address| m.board.read32(address).unwrap()),
        sem: [
            target.sem[0],
            target.sem[0] + 4,
            target.sem[1],
            target.sem[1] + 4,
        ]
        .map(|address| m.board.read32(address).unwrap()),
        queue: [
            READY_HEAD,
            WAITER,
            WAITER + 4,
            WAITER + 8,
            target.high_ready,
        ]
        .map(|address| m.board.read32(address).unwrap()),
        forced: m.board.read32(INTC2 + 0x14).unwrap(),
        irqstat: m.board.read32(IRQSTAT).unwrap(),
        signal_entries,
    }
}

#[test]
fn actual_firmware_delivery_readies_waiter_and_preserves_stock_results() {
    for target in DELIVERY_TARGETS {
        let Some(path) = std::env::var_os(target.env) else {
            eprintln!(
                "SKIP controlled {} eSDHC delivery/waiter proof: set {} to its stock .syx",
                target.name, target.env
            );
            continue;
        };
        let raw = std::fs::read(path).unwrap_or_else(|error| {
            panic!(
                "configured {} must name a readable stock image: {error}",
                target.env
            )
        });
        let fw = Firmware::parse(&raw).expect("parse configured stock image");
        assert_eq!(
            fw.syx_sha256, target.sha,
            "requires exact stock {}",
            target.name
        );
        let linked = compiled_at(target.sites);
        // Every case includes command-generated CC; data cases additionally
        // drive explicit immediate Device events without DMA or physical timing.
        for (extra_irq, waiter_sem) in
            [(0, 1), (0x0001_0000, 1), (0x0010_0002, 0), (0x0020_0002, 0)]
        {
            let stock = delivered_replay(&fw, &linked, &target, false, extra_irq, waiter_sem);
            let patched = delivered_replay(&fw, &linked, &target, true, extra_irq, waiter_sem);
            assert_eq!(
                patched,
                stock,
                "{} IRQ={:#x}: observer preserves status, queue and context",
                target.name,
                extra_irq | 1
            );
        }
    }
}

// A controlled synchronous completion followed by optional error status bits:
// this exercises actual firmware handlers, not card corruption, DMA abort,
// interrupt timing, scheduler execution, native boot or sample-file loading.
const PAIR_DEST: u32 = 0x4200_0000;
const PAIR_CEILING: usize = 4096;
const DMA_VECTOR: u32 = 192;
const DMA_PENDING: u32 = 1 << 27;
// MCF5441x eDMA59 descriptor and request/latch registers. Keep these fixture
// inputs explicit rather than adding another test dependency.
const PAIR_TCD: u32 = 0xfc04_5000 + 59 * 0x20;
const PAIR_SERQ: u32 = 0xfc04_4018;
const PAIR_INTH: u32 = 0xfc04_4020;
const PAIR_XFERTYP: u32 = 0xfc0c_c00c;
const PAIR_DATPORT: u32 = 0xfc0c_c020;

struct PairMedia;

fn pair_byte(offset: usize) -> u8 {
    (offset as u8).wrapping_mul(37).wrapping_add(11)
}

impl emmc_card::RandomAccessRead for PairMedia {
    fn len(&self) -> u64 {
        512
    }

    fn read_at(&self, offset: u64, destination: &mut [u8]) -> usize {
        let available = 512u64.saturating_sub(offset).min(destination.len() as u64) as usize;
        for (i, byte) in destination[..available].iter_mut().enumerate() {
            *byte = pair_byte(offset as usize + i);
        }
        available
    }
}

#[derive(Debug, PartialEq, Eq)]
struct PairResult {
    d: [u32; 8],
    a: [u32; 8],
    sr: u16,
    pc: u32,
    other_a7: u32,
    status: [u32; 2],
    sem: [u32; 6],
    queue: [u32; 11],
    forced: u32,
    irqstat: u32,
    inth: u32,
    entries: Vec<u32>,
    rtes: Vec<u32>,
    signal_entries: [usize; 2],
    payload: Vec<u8>,
}

fn pair_payload(m: &mut machine::Machine) -> Vec<u8> {
    (0..512)
        .map(|i| m.board.read8(PAIR_DEST + i).unwrap())
        .collect()
}

fn pair_replay(
    fw: &Firmware,
    linked: &Linked,
    target: &DeliveryTarget,
    patched: bool,
    dma_level: u8,
    extra_irq: u32,
) -> PairResult {
    use emmc_card::{Card, CardIdentity};

    let mut m = delivery_machine(fw, linked, target, patched, 0);
    *m.board.esdhc.card_mut() =
        Card::with_backing(CardIdentity::Mlc4G, Some(Box::new(PairMedia))).unwrap();
    m.board.map_ram_page(PAIR_DEST).unwrap();
    m.board
        .write32(VBR + DMA_VECTOR * 4, target.dma_isr)
        .unwrap();
    m.board.write8(INTC2 + 0x40, dma_level).unwrap();
    m.board.write8(INTC2 + 0x1d, 0).unwrap();
    let sems = [target.sem[0], target.sem[1], target.dma_sem];
    let waiters = [WAITER, WAITER + 0x40, WAITER + 0x80];
    for (sem, waiter) in sems.into_iter().zip(waiters) {
        m.board.write32(sem, 0).unwrap();
        m.board.write32(sem + 4, waiter).unwrap();
        m.board.write32(waiter, 0).unwrap();
        m.board.write32(waiter + 4, 0).unwrap();
        m.board.write32(waiter + 8, READY_HEAD).unwrap();
    }
    m.board.write32(READY_HEAD, 0).unwrap();
    m.board.write32(target.high_ready, 0).unwrap();
    let tcd = PAIR_TCD;
    m.board.write32(tcd, PAIR_DATPORT).unwrap();
    m.board.write32(tcd + 0x10, PAIR_DEST).unwrap();
    m.board.write32(tcd + 0x08, 512).unwrap();
    m.board.write16(tcd + 0x14, 1).unwrap();
    m.board.write16(tcd + 0x1c, 1).unwrap();
    m.board.write16(tcd + 0x1e, 0x000a).unwrap();
    // MOVE.B D1,(SERQ).L; MOVE.L D0,(XFERTYP).L; NOP.
    m.board.write16(RETURN, 0x13c1).unwrap();
    m.board.write32(RETURN + 2, PAIR_SERQ).unwrap();
    m.board.write16(RETURN + 6, 0x23c0).unwrap();
    m.board.write32(RETURN + 8, PAIR_XFERTYP).unwrap();
    m.board.write16(RETURN + 12, 0x4e71).unwrap();
    m.cpu.d[1] = 59;
    m.cpu.d[0] = (18 << 24) | (1 << 21) | (1 << 4);
    assert!(m.step_storage().unwrap().is_empty());
    let events = m.step_storage().unwrap();
    assert!(events.iter().any(|event| matches!(event, machine::CompletionEvent::Dma59 { completion, .. } if completion.done && completion.major_interrupt && completion.disable_request)));
    assert_eq!(m.irqs_delivered, 0);
    assert_eq!(m.cpu.pc, RETURN + 12);
    let expected_payload: Vec<u8> = (0..512).map(pair_byte).collect();
    assert_eq!(pair_payload(&mut m), expected_payload);
    assert_eq!(m.board.read32(IRQSTAT).unwrap(), 3);
    assert_eq!(m.board.read32(PAIR_INTH).unwrap(), DMA_PENDING);
    for (sem, waiter) in sems.into_iter().zip(waiters) {
        assert_eq!(
            m.board.read32(sem).unwrap(),
            0,
            "Device does not host-post signals"
        );
        assert_eq!(m.board.read32(sem + 4).unwrap(), waiter);
    }
    assert_eq!(m.board.read32(READY_HEAD).unwrap(), 0);
    assert_eq!(m.board.read32(target.high_ready).unwrap(), 0);
    if extra_irq != 0 {
        m.board.esdhc.device_event(extra_irq).unwrap();
    }
    let irq = 3 | extra_irq;
    assert_eq!(m.board.read32(IRQSTAT).unwrap(), irq);
    m.cpu.resolve_nzv();
    let before = (m.cpu.d, m.cpu.a, m.cpu.sr, m.cpu.other_a7);
    let mut entries = Vec::new();
    let mut rtes = Vec::new();
    let mut signal_entries = [0; 2];
    let mut returned = false;
    // Vector-table reads independently identify the actual exception entries.
    m.board.set_guest_access_capture(true);
    for _ in 0..PAIR_CEILING {
        if m.irqs_delivered == 2 && m.cpu.pc == RETURN + 12 {
            returned = true;
            break;
        }
        if m.cpu.pc == target.rte {
            rtes.push(DELIVERY_VECTOR);
        }
        if m.cpu.pc == target.dma_rte {
            rtes.push(DMA_VECTOR);
        }
        if m.cpu.pc == 0x4000_155c {
            signal_entries[0] += 1;
        }
        if m.cpu.pc == 0x4000_11ee {
            signal_entries[1] += 1;
        }
        let pc = m.cpu.pc;
        m.step_storage().unwrap_or_else(|error| {
            panic!(
                "{} patched={patched} DMA level={dma_level} IRQ={irq:#x} at {pc:#x}: {error:?}",
                target.name
            )
        });
        assert_eq!(
            m.cpu.last_exception, None,
            "unexpected instruction exception at {pc:#x}"
        );
        for read in m.board.take_guest_reads() {
            for vector in [DELIVERY_VECTOR, DMA_VECTOR] {
                if read.address == VBR + vector * 4 {
                    assert_eq!(read.size, 4);
                    entries.push(vector);
                }
            }
        }
        m.board.take_guest_writes();
    }
    m.board.set_guest_access_capture(false);
    assert!(
        returned,
        "{} pair exceeded {PAIR_CEILING} instructions at {:#x}",
        target.name, m.cpu.pc
    );
    m.cpu.resolve_nzv();
    assert_eq!(
        (m.cpu.d, m.cpu.a, m.cpu.sr, m.cpu.other_a7),
        before,
        "both ISRs preserve context"
    );
    let order = if dma_level == 3 {
        vec![DELIVERY_VECTOR, DMA_VECTOR]
    } else {
        vec![DMA_VECTOR, DELIVERY_VECTOR]
    };
    assert_eq!(
        entries, order,
        "one entry to each actual ISR, ordered by INTC2"
    );
    assert_eq!(rtes, order, "one actual RTE per ISR");
    assert_eq!(m.irqs_delivered, 2);
    assert_eq!(signal_entries, [(irq.count_ones() + 1) as usize; 2]);
    assert_eq!(
        m.board.read32(PAIR_INTH).unwrap(),
        0,
        "actual DMA ISR CINT acknowledges channel59"
    );
    assert_eq!(m.board.read32(IRQSTAT).unwrap(), 0, "actual eSDHC ISR W1C");
    assert!(!m.board.esdhc.interrupt_pending());
    assert_eq!(m.board.storage_dma_pending(), 0);
    let signal_counts = [
        (irq & 0x0030_0002).count_ones(),
        (irq & 0x0001_0001).count_ones(),
        1,
    ];
    for (sem, count) in sems.into_iter().zip(signal_counts) {
        assert_eq!(
            m.board.read32(sem + 4).unwrap(),
            0,
            "waiter removed exactly once"
        );
        assert_eq!(
            m.board.read32(sem).unwrap(),
            (count - 1).min(1),
            "first signal wakes waiter; later signals coalesce"
        );
    }
    // Validate a circular doubly-linked ready list containing all three
    // distinct waiters once, including the nonempty insertion path.
    let head = m.board.read32(READY_HEAD).unwrap();
    assert!(waiters.contains(&head));
    let mut seen = Vec::new();
    let mut node = head;
    for _ in 0..waiters.len() {
        assert!(waiters.contains(&node));
        assert!(!seen.contains(&node), "waiter duplicated in ready queue");
        seen.push(node);
        let next = m.board.read32(node).unwrap();
        let previous = m.board.read32(node + 4).unwrap();
        assert_eq!(m.board.read32(next + 4).unwrap(), node);
        assert_eq!(m.board.read32(previous).unwrap(), node);
        node = next;
    }
    assert_eq!(node, head);
    assert_eq!(m.board.read32(target.high_ready).unwrap(), READY_HEAD);
    let forced = m.board.read32(INTC2 + 0x14).unwrap();
    assert_eq!(
        forced & (1 << 13),
        1 << 13,
        "scheduling requested, never executed"
    );
    assert_eq!(
        m.board.read32(target.status[1]).unwrap(),
        0,
        "CC follows CTOE"
    );
    assert_eq!(
        m.board.read32(target.status[0]).unwrap(),
        if extra_irq & 0x0030_0000 != 0 {
            u32::MAX
        } else {
            0
        }
    );
    assert_eq!(
        m.board
            .read32(linked.symbols["storage_fault_bits"] as u32)
            .unwrap(),
        if patched { irq & MASK } else { 0 }
    );
    let payload = pair_payload(&mut m);
    assert_eq!(
        payload, expected_payload,
        "handlers leave the complete DMA payload unchanged"
    );
    PairResult {
        d: m.cpu.d,
        a: m.cpu.a,
        sr: m.cpu.sr,
        pc: m.cpu.pc,
        other_a7: m.cpu.other_a7,
        status: target.status.map(|at| m.board.read32(at).unwrap()),
        sem: [
            sems[0],
            sems[0] + 4,
            sems[1],
            sems[1] + 4,
            sems[2],
            sems[2] + 4,
        ]
        .map(|at| m.board.read32(at).unwrap()),
        queue: [
            READY_HEAD,
            waiters[0],
            waiters[0] + 4,
            waiters[0] + 8,
            waiters[1],
            waiters[1] + 4,
            waiters[1] + 8,
            waiters[2],
            waiters[2] + 4,
            waiters[2] + 8,
            target.high_ready,
        ]
        .map(|at| m.board.read32(at).unwrap()),
        forced,
        irqstat: m.board.read32(IRQSTAT).unwrap(),
        inth: m.board.read32(PAIR_INTH).unwrap(),
        entries,
        rtes,
        signal_entries,
        payload,
    }
}

#[test]
fn actual_storage_pair_orders_both_isrs_and_readies_three_waiters() {
    for target in DELIVERY_TARGETS {
        let Some(path) = std::env::var_os(target.env) else {
            eprintln!(
                "SKIP controlled {} storage pair proof: set {} to its stock .syx",
                target.name, target.env
            );
            continue;
        };
        let raw = std::fs::read(path).expect("configured stock image must be readable");
        let fw = Firmware::parse(&raw).expect("parse configured stock image");
        assert_eq!(
            fw.syx_sha256, target.sha,
            "requires exact stock {}",
            target.name
        );
        // DN2 relocations are test-only; no packaged firmware port is added.
        let linked = compiled_at(target.sites);
        for (dma_level, extra_irq) in [(3, 0), (4, 0), (3, 0x0011_0000), (4, 0x0020_0000)] {
            let stock = pair_replay(&fw, &linked, &target, false, dma_level, extra_irq);
            let patched = pair_replay(&fw, &linked, &target, true, dma_level, extra_irq);
            assert_eq!(
                patched, stock,
                "{} DMA level={dma_level} extra IRQ={extra_irq:#x}: observer preserves selected stock results",
                target.name
            );
        }
    }
}

// Controlled continuation into one explicitly initialized task, not a restored
// native boot, a complete scheduler/timer model or a sample-file load.
const SCHEDULER: u32 = 0x4000_0410;
const SCHEDULER_RTE: u32 = 0x4000_0458;
const SCHEDULER_VECTOR: u32 = 205;
const OLD_TASK: u32 = 0x4100_0300;
const TASK_SP: u32 = 0x4100_8000;
const TASK_PC: u32 = 0xa000;
const TASK_MARKER: u32 = 0xb000;
const TASK_SR: u16 = 0x2015;

#[derive(Debug, PartialEq, Eq)]
struct SchedulerResult {
    d: [u32; 8],
    a: [u32; 8],
    sr: u16,
    pc: u32,
    saved: Vec<u32>,
    marker: u32,
    status: [u32; 2],
    sem: [u32; 4],
    queue: [u32; 5],
    forced: u32,
    irqstat: u32,
    entries: Vec<u32>,
    rtes: Vec<u32>,
    signal_entries: [usize; 2],
}

fn scheduler_replay(
    fw: &Firmware,
    linked: &Linked,
    target: &DeliveryTarget,
    patched: bool,
    extra_irq: u32,
) -> SchedulerResult {
    let mut m = delivery_machine(fw, linked, target, patched, 1);
    const PIT0_PCSR: u32 = 0xfc08_0000;
    // Explicit supplied timeout, with PIE disabled: the storage/force route
    // does not deliver physical PIT requests. Only actual guest code may ack PIF.
    m.board.write16(PIT0_PCSR, 0).unwrap();
    m.board.time_mut().unwrap().device_pit_timeout(0).unwrap();
    assert_eq!(m.board.read16(PIT0_PCSR).unwrap(), 4);
    let current_task = if target.sha == STOCK_SHA {
        0x47db_3f64
    } else {
        0x4664_bcdc
    };
    m.board.map_ram_page(current_task & !0x000f_ffff).unwrap();
    m.board.write32(current_task, OLD_TASK).unwrap();
    m.board
        .write32(VBR + SCHEDULER_VECTOR * 4, SCHEDULER)
        .unwrap();
    m.board.write8(INTC2 + 0x4d, 1).unwrap();
    m.board
        .write32(INTC2 + 0x0c, !((1 << 31) | (1 << 13)))
        .unwrap();
    // These are the actual stock handler opcodes checked in both current
    // images, not a substitute scheduler implemented by the fixture.
    assert_eq!(m.board.read32(SCHEDULER).unwrap(), 0x46fc_2700);
    assert_eq!(m.board.read16(SCHEDULER_RTE).unwrap(), 0x4e73);
    let task_d = std::array::from_fn::<_, 8, _>(|i| 0x5160_1000 + i as u32 * 0x101);
    let mut task_a = std::array::from_fn::<_, 8, _>(|i| 0x6270_2000 + i as u32 * 0x101);
    task_a[7] = TASK_SP;
    // MCF5441x format-4 frame: format/vector/SR, then saved PC. The selected
    // task's MOVEM slot contains the frame SP, whereas its post-RTE SP is +8.
    let frame_sp = TASK_SP - 8;
    m.board
        .write32(
            frame_sp,
            0x4000_0000 | (SCHEDULER_VECTOR << 18) | u32::from(TASK_SR),
        )
        .unwrap();
    m.board.write32(frame_sp + 4, TASK_PC).unwrap();
    for (i, value) in task_d.into_iter().chain(task_a).enumerate() {
        m.board
            .write32(
                WAITER + 0x0c + i as u32 * 4,
                if i == 15 { frame_sp } else { value },
            )
            .unwrap();
    }
    m.board.write16(TASK_PC, 0x23c3).unwrap(); // MOVE.L D3,(TASK_MARKER).L
    m.board.write32(TASK_PC + 2, TASK_MARKER).unwrap();
    m.board.write16(TASK_PC + 6, 0x4e71).unwrap();
    m.board.write32(TASK_MARKER, 0).unwrap();
    m.cpu.sr = 0x2015; // IPL 0 permits level-1 scheduling after level-3 storage
    assert!(
        m.step_storage_with_forced()
            .unwrap()
            .iter()
            .all(|event| !matches!(event, machine::CompletionEvent::Dma59 { .. }))
    ); // guest CMD13
    assert_eq!(m.cpu.pc, RETURN + 6);
    assert_eq!(m.irqs_delivered, 0);
    assert_eq!(m.board.read32(IRQSTAT).unwrap(), 1);
    assert_eq!(
        m.board.read32(target.sem[1]).unwrap(),
        0,
        "no host semaphore post"
    );
    assert_eq!(m.board.read32(target.sem[1] + 4).unwrap(), WAITER);
    assert_eq!(m.board.read32(READY_HEAD).unwrap(), 0);
    assert_eq!(m.board.read32(target.high_ready).unwrap(), 0);
    assert_eq!(m.board.read32(TASK_MARKER).unwrap(), 0);
    if extra_irq != 0 {
        m.board.esdhc.device_event(extra_irq).unwrap();
    }
    m.cpu.resolve_nzv();
    let before = (m.cpu.d, m.cpu.a, m.cpu.sr, m.cpu.other_a7);
    let mut entries = Vec::new();
    let mut rtes = Vec::new();
    let mut signal_entries = [0; 2];
    let mut resumed = false;
    m.board.set_guest_access_capture(true);
    for _ in 0..PAIR_CEILING {
        let pc = m.cpu.pc;
        assert_eq!(
            m.board.read32(TASK_MARKER).unwrap(),
            0,
            "task has not executed before scheduler RTE"
        );
        if pc == target.rte {
            rtes.push(DELIVERY_VECTOR);
        }
        if pc == SCHEDULER_RTE {
            rtes.push(SCHEDULER_VECTOR);
            assert_eq!(
                m.cpu.d, task_d,
                "actual MOVEM restores selected D registers"
            );
            let mut frame_a = task_a;
            frame_a[7] = frame_sp;
            assert_eq!(
                m.cpu.a, frame_a,
                "actual MOVEM restores selected A registers and frame SP"
            );
        }
        if pc == 0x4000_155c {
            signal_entries[0] += 1;
        }
        if pc == 0x4000_11ee {
            signal_entries[1] += 1;
        }
        m.step_storage_with_forced().unwrap_or_else(|error| {
            panic!(
                "{} patched={patched} scheduler IRQ={:#x} at {pc:#x}: {error:?}",
                target.name,
                extra_irq | 1
            )
        });
        assert_eq!(
            m.cpu.last_exception, None,
            "unexpected instruction exception at {pc:#x}"
        );
        for read in m.board.take_guest_reads() {
            for vector in [DELIVERY_VECTOR, SCHEDULER_VECTOR] {
                if read.address == VBR + vector * 4 {
                    assert_eq!(read.size, 4);
                    entries.push(vector);
                }
            }
        }
        m.board.take_guest_writes();
        if pc == target.rte {
            m.cpu.resolve_nzv();
            assert_eq!(
                (m.cpu.d, m.cpu.a, m.cpu.sr, m.cpu.other_a7),
                before,
                "storage ISR preserves source task context"
            );
            assert_eq!(m.cpu.pc, RETURN + 6);
            assert_eq!(
                m.board.read16(PIT0_PCSR).unwrap() & 4,
                4,
                "storage ISR entry/RTE does not acknowledge PIT status"
            );
            assert_eq!(
                m.board.read32(INTC2 + 0x14).unwrap() & (1 << 13),
                1 << 13,
                "actual signal helper requests scheduler"
            );
        }
        if pc == SCHEDULER_RTE {
            assert_eq!(
                m.board.read16(PIT0_PCSR).unwrap(),
                0x053b,
                "actual scheduler PCSR $053f word write acknowledges PIF"
            );
            m.cpu.resolve_nzv();
            assert_eq!(m.cpu.d, task_d);
            assert_eq!(m.cpu.a, task_a);
            assert_eq!(m.cpu.sr, TASK_SR);
            assert_eq!(m.cpu.pc, TASK_PC);
            assert_eq!(m.cpu.other_a7, before.3);
            assert_eq!(
                m.board.read32(TASK_MARKER).unwrap(),
                0,
                "RTE precedes sentinel execution"
            );
            resumed = true;
            break;
        }
    }
    assert!(
        resumed,
        "{} scheduler exceeded {PAIR_CEILING} instructions at {:#x}",
        target.name, m.cpu.pc
    );
    assert_eq!(entries, [DELIVERY_VECTOR, SCHEDULER_VECTOR]);
    assert_eq!(rtes, [DELIVERY_VECTOR, SCHEDULER_VECTOR]);
    assert_eq!(m.irqs_delivered, 2);
    assert_eq!(signal_entries, [(extra_irq | 1).count_ones() as usize; 2]);
    let saved: Vec<u32> = (0..16)
        .map(|i| m.board.read32(OLD_TASK + 0x0c + i * 4).unwrap())
        .collect();
    let mut expected_a = before.1;
    expected_a[7] -= 8; // saved source context points at its scheduler IRQ frame
    assert_eq!(
        saved,
        before.0.into_iter().chain(expected_a).collect::<Vec<_>>(),
        "actual MOVEM saves old task, including original A0 and frame SP"
    );
    assert_eq!(
        m.board.read32(before.1[7] - 8).unwrap(),
        0x4000_0000 | (SCHEDULER_VECTOR << 18) | u32::from(before.2)
    );
    assert_eq!(m.board.read32(before.1[7] - 4).unwrap(), RETURN + 6);
    assert_eq!(
        target.status.map(|at| m.board.read32(at).unwrap()),
        [0, 0],
        "CC clears command status after any CTOE"
    );
    assert_eq!(m.board.read32(current_task).unwrap(), WAITER);
    assert_eq!(m.board.read32(READY_HEAD).unwrap(), WAITER);
    assert_eq!(m.board.read32(WAITER).unwrap(), WAITER);
    assert_eq!(m.board.read32(WAITER + 4).unwrap(), WAITER);
    assert_eq!(m.board.read32(WAITER + 8).unwrap(), READY_HEAD);
    assert_eq!(m.board.read32(target.high_ready).unwrap(), READY_HEAD);
    assert_eq!(m.board.read32(target.sem[1] + 4).unwrap(), 0);
    assert_eq!(
        m.board.read32(target.sem[1]).unwrap(),
        u32::from(extra_irq != 0)
    );
    assert_eq!(m.board.read32(target.sem[0]).unwrap(), 0);
    assert_eq!(m.board.read32(target.sem[0] + 4).unwrap(), 0);
    assert_eq!(m.board.read32(IRQSTAT).unwrap(), 0);
    assert!(!m.board.esdhc.interrupt_pending());
    assert_eq!(
        m.board.read32(INTC2 + 0x14).unwrap(),
        0,
        "actual scheduler AND clears bit 13"
    );
    assert_eq!(
        m.board
            .read32(linked.symbols["storage_fault_bits"] as u32)
            .unwrap(),
        if patched { extra_irq & MASK } else { 0 }
    );
    m.board.set_guest_access_capture(false);
    m.step_storage_with_forced().unwrap(); // ordinary resumed-task instruction
    assert_eq!(m.cpu.last_exception, None);
    assert_eq!(m.cpu.pc, TASK_PC + 6);
    assert_eq!(m.board.read32(TASK_MARKER).unwrap(), task_d[3]);
    assert_eq!(m.irqs_delivered, 2);
    m.cpu.resolve_nzv();
    assert_eq!(m.cpu.d, task_d);
    assert_eq!(m.cpu.a, task_a);
    assert_eq!(m.cpu.sr, 0x2010); // MOVE of positive nonzero long preserves X
    SchedulerResult {
        d: m.cpu.d,
        a: m.cpu.a,
        sr: m.cpu.sr,
        pc: m.cpu.pc,
        saved,
        marker: m.board.read32(TASK_MARKER).unwrap(),
        status: target.status.map(|at| m.board.read32(at).unwrap()),
        sem: [
            target.sem[0],
            target.sem[0] + 4,
            target.sem[1],
            target.sem[1] + 4,
        ]
        .map(|at| m.board.read32(at).unwrap()),
        queue: [
            READY_HEAD,
            WAITER,
            WAITER + 4,
            WAITER + 8,
            target.high_ready,
        ]
        .map(|at| m.board.read32(at).unwrap()),
        forced: m.board.read32(INTC2 + 0x14).unwrap(),
        irqstat: m.board.read32(IRQSTAT).unwrap(),
        entries,
        rtes,
        signal_entries,
    }
}

#[test]
fn actual_storage_isr_schedules_initialized_waiter_and_executes_guest_task() {
    for target in DELIVERY_TARGETS {
        let Some(path) = std::env::var_os(target.env) else {
            eprintln!(
                "SKIP controlled {} scheduler continuation: set {} to its stock .syx",
                target.name, target.env
            );
            continue;
        };
        let raw = std::fs::read(path).expect("configured stock image must be readable");
        let fw = Firmware::parse(&raw).expect("parse configured stock image");
        assert_eq!(
            fw.syx_sha256, target.sha,
            "requires exact stock {}",
            target.name
        );
        let linked = compiled_at(target.sites);
        for extra_irq in [0, 0x0001_0000] {
            let stock = scheduler_replay(&fw, &linked, &target, false, extra_irq);
            let patched = scheduler_replay(&fw, &linked, &target, true, extra_irq);
            assert_eq!(
                patched,
                stock,
                "{} scheduler IRQ={:#x}: observer preserves selected results",
                target.name,
                extra_irq | 1
            );
        }
    }
}

#[test]
fn actual_dtim3_handler_prefix_acknowledges_reference_and_preserves_capture() {
    for target in DELIVERY_TARGETS {
        let Some(path) = std::env::var_os(target.env) else {
            eprintln!(
                "SKIP controlled {} DTIM prefix: set {} to its stock .syx",
                target.name, target.env
            );
            continue;
        };
        let raw = std::fs::read(path).expect("configured stock image must be readable");
        let fw = Firmware::parse(&raw).expect("parse configured stock image");
        assert_eq!(fw.syx_sha256, target.sha, "requires exact stock image");
        let linked = compiled_at(target.sites);
        let (handler, helper_call) = if target.sha == STOCK_SHA {
            (0x400c_0b40, 0x400c_0b5c)
        } else {
            (0x400b_b170, 0x400b_b18c)
        };
        for capture in [0_u8, 1] {
            const DTIM3: u32 = 0xfc07_c000;
            let mut m = delivery_machine(&fw, &linked, &target, false, 1);
            // Test-only preexisting CAP. No capture event generation is modeled.
            let mut page = [0; 16];
            page[3] = capture;
            assert!(m.board.time_mut().unwrap().load_page(DTIM3, &page));
            m.board.write32(DTIM3 + 12, 0).unwrap();
            m.board.write32(DTIM3 + 4, 0x43238).unwrap();
            m.board.write16(DTIM3, 0x001d).unwrap();
            m.board.time_mut().unwrap().device_dtim_reference(3).unwrap();
            assert_eq!(m.board.read8(DTIM3 + 3).unwrap(), capture | 2);
            assert_eq!(m.board.read16(helper_call).unwrap(), 0x4eb9);
            assert_eq!(m.board.read32(helper_call + 2).unwrap(), 0x4000_1896);
            m.board.write32(VBR + 99 * 4, handler).unwrap();
            m.cpu.sr = 0x2000;
            m.cpu.take_interrupt(&mut m.board, 99, Some(2), InterruptPolicy::Device).unwrap();
            assert_eq!(m.cpu.pc, handler);
            assert_eq!(m.board.read8(DTIM3 + 3).unwrap(), capture | 2);
            m.board.set_guest_access_capture(true);
            let mut acknowledged = false;
            for _ in 0..64 {
                if m.cpu.pc == helper_call {
                    break; // queue helper/task context is deliberately outside this proof
                }
                m.cpu.step(&mut m.board).unwrap();
                assert_eq!(m.cpu.last_exception, None);
                for write in m.board.take_guest_writes() {
                    if write.address == DTIM3 + 3 {
                        assert_eq!((write.size, write.value), (1, 2));
                        acknowledged = true;
                    }
                }
            }
            assert_eq!(m.cpu.pc, helper_call, "bounded actual handler prefix");
            assert!(acknowledged, "actual DTER REF byte acknowledgement observed");
            assert_eq!(m.board.read8(DTIM3 + 3).unwrap(), capture);
            assert!(m.board.time().unwrap().device_dtim_reference_requests().unwrap().is_empty());
        }
    }
}
