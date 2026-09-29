#![cfg(feature = "trace")]

//! Opt-in local oracle fixture gate. Firmware-derived traces and card images
//! are provided only through environment variables and are never committed.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::sync::Mutex;

use coldfire::Bus;
use emmc_card::{Card, DEFAULT_CAPACITY_BLOCKS, RandomAccessRead, SMALL_CAPACITY_BLOCKS};
use machine::board::{Board, CompletionPolicy, SemaphoreAddresses};
use periph::{edma, esdhc, trace};

struct LocalImage {
    file: Mutex<File>,
    size: u64,
}
impl LocalImage {
    fn open(path: &str) -> Self {
        let file = File::open(path).unwrap();
        let size = file.metadata().unwrap().len();
        Self {
            file: Mutex::new(file),
            size,
        }
    }
}
impl RandomAccessRead for LocalImage {
    fn len(&self) -> u64 {
        self.size
    }
    fn read_at(&self, pos: u64, dst: &mut [u8]) -> usize {
        let mut file = self.file.lock().unwrap();
        file.seek(SeekFrom::Start(pos)).unwrap();
        file.read(dst).unwrap()
    }
}

fn armed_read_command(addr: u32, size: u8, value: u32, armed: bool) -> Option<u8> {
    let command = ((value >> 24) & 63) as u8;
    (addr == esdhc::BASE + esdhc::XFERTYP
        && size == 4
        && armed
        && matches!(command, 8 | 18)
        && value & (1 << 21) != 0
        && value & (1 << 4) != 0)
        .then_some(command)
}

fn gate(trace_path: &str, card_path: Option<&str>, capacity: u32, until_first_write: bool) {
    let card = Card::with_backing(
        capacity,
        card_path.map(|p| Box::new(LocalImage::open(p)) as Box<dyn RandomAccessRead>),
    )
    .unwrap();
    let mut board = Board::new(
        card,
        SemaphoreAddresses::default(),
        CompletionPolicy::Oracle,
    );
    let mut reader = trace::Reader::open(trace_path).unwrap();
    assert_eq!(reader.version, 2, "late gate needs live SR trace");
    let mut pending_reads = VecDeque::new();
    let mut data_outputs = 0;
    let mut issued_cmd18 = 0;
    let mut reached_write = false;
    while let Some(record) = reader.next_record().unwrap() {
        match record.tag {
            trace::PAGE => {
                let base = record.u32(0);
                board.dma.load_page(base, &record.data);
                board.esdhc.load_page(base, &record.data);
            }
            trace::WR => {
                let addr = record.u32(0);
                let value = record.u32(1);
                let size = record.u8(3);
                if periph::spilink::DmaLink::owns(addr) || esdhc::Esdhc::<Card>::owns(addr) {
                    if addr == esdhc::BASE + esdhc::XFERTYP
                        && size == 4
                        && (value >> 24) & 63 == 25
                        && until_first_write
                    {
                        assert!(
                            pending_reads.is_empty(),
                            "CMD25 reached with unread CMD8/CMD18 output"
                        );
                        reached_write = true;
                        break;
                    }
                    let candidate = armed_read_command(addr, size, value, board.dma59_armed());
                    board.write_guest(addr, size, value).unwrap();
                    if let Some(command) = candidate {
                        assert!(
                            !board.dma59_armed(),
                            "CMD{command} did not service the armed DMA59 transfer"
                        );
                        pending_reads.push_back(command);
                        issued_cmd18 += u32::from(command == 18);
                    }
                }
            }
            trace::HWR => {
                let source = reader
                    .sources
                    .get(&record.u16(0))
                    .map(String::as_str)
                    .unwrap_or("");
                if source == "emu.esdhc.Esdhc._dma_out" && record.data.len() >= 512 {
                    let command = pending_reads
                        .pop_front()
                        .expect("DMA output without an armed CMD8/CMD18");
                    if command == 18 {
                        let base = record.u32(1);
                        for (i, &expected) in record.data.iter().enumerate() {
                            assert_eq!(
                                board.read8(base + i as u32).unwrap(),
                                expected,
                                "CMD18 guest byte offset {i} clock {}",
                                record.clock
                            );
                        }
                        let t = edma::TcdView::new(&mut board.dma.edma_regs, esdhc::DMA_CHANNEL)
                            .snapshot();
                        assert_eq!(t.daddr, base + record.data.len() as u32);
                        data_outputs += 1;
                        if data_outputs == 2 && !until_first_write {
                            assert!(
                                pending_reads.is_empty(),
                                "stopped with unread CMD8/CMD18 output"
                            );
                            break;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    assert!(
        pending_reads.is_empty(),
        "unmatched read transfer at trace stop/EOF"
    );
    assert_eq!(
        data_outputs, issued_cmd18,
        "each serviced CMD18 needs its own output"
    );
    let minimum = 2;
    assert!(issued_cmd18 >= minimum);
    assert!(
        data_outputs >= minimum,
        "expected independent CMD18 payloads"
    );
    if until_first_write {
        assert!(reached_write, "CMD25 boundary not reached");
        assert!(
            pending_reads.is_empty(),
            "unread CMD8/CMD18 output at CMD25 boundary"
        );
        assert_eq!(data_outputs, issued_cmd18, "each CMD18 must move data");
        eprintln!("{trace_path}: {data_outputs} CMD18 payloads checked before CMD25");
    }
}

fn cmd25_gate(trace_path: &str, card_path: Option<&str>, capacity: u32, full: bool) {
    let card = Card::with_backing(
        capacity,
        card_path.map(|p| Box::new(LocalImage::open(p)) as Box<dyn RandomAccessRead>),
    )
    .unwrap();
    let mut board = Board::new(
        card,
        SemaphoreAddresses::default(),
        CompletionPolicy::Oracle,
    );
    let mut reader = trace::Reader::open(trace_path).unwrap();
    assert_eq!(reader.version, 2);
    let mut pending_reads = VecDeque::new();
    let mut issued_cmd18 = 0;
    let mut pending_xfer = None;
    let mut argument = 0u32;
    let mut tcd_writes_checked = 0;
    let mut input_bytes = 0usize;
    let mut writes_issued = 0;
    let mut writes_confirmed = 0;
    let mut reads_confirmed = 0;
    let mut clean_end = false;
    while let Some(record) = reader.next_record().unwrap() {
        match record.tag {
            trace::PAGE => {
                let base = record.u32(0);
                board.dma.load_page(base, &record.data);
                board.esdhc.load_page(base, &record.data);
            }
            trace::WR => {
                let addr = record.u32(0);
                let value = record.u32(1);
                let size = record.u8(3);
                if addr == esdhc::BASE + esdhc::CMDARG && size == 4 {
                    argument = value;
                }
                if periph::spilink::DmaLink::owns(addr) || esdhc::Esdhc::<Card>::owns(addr) {
                    if addr == esdhc::BASE + esdhc::XFERTYP && size == 4 && (value >> 24) & 63 == 25
                    {
                        assert!(
                            pending_reads.is_empty(),
                            "CMD25 overtook a pending card-to-guest output"
                        );
                        pending_xfer = Some(value);
                    } else {
                        let candidate = armed_read_command(addr, size, value, board.dma59_armed());
                        board.write_guest(addr, size, value).unwrap();
                        if let Some(command) = candidate {
                            assert!(
                                !board.dma59_armed(),
                                "CMD{command} did not service the armed DMA59 transfer"
                            );
                            pending_reads.push_back(command);
                            issued_cmd18 += u32::from(command == 18);
                        }
                    }
                }
            }
            trace::HRD => {
                let source = reader
                    .sources
                    .get(&record.u16(0))
                    .map(String::as_str)
                    .unwrap_or("");
                if pending_xfer.is_some()
                    && source == "emu.esdhc.Esdhc._dma_in"
                    && record.data.len() >= 512
                {
                    let t =
                        edma::TcdView::new(&mut board.dma.edma_regs, esdhc::DMA_CHANNEL).snapshot();
                    assert_eq!(record.data.len(), t.citer as usize * t.nbytes as usize);
                    let mut src = i64::from(t.saddr);
                    for chunk in record.data.chunks_exact(t.nbytes as usize) {
                        let start = u32::try_from(src).unwrap();
                        for (i, &byte) in chunk.iter().enumerate() {
                            let addr = start + i as u32;
                            board.map_ram_page(addr & !((1 << 20) - 1)).unwrap();
                            board.write8(addr, byte).unwrap();
                        }
                        src += i64::from(t.soff);
                    }
                    board
                        .write_guest(
                            esdhc::BASE + esdhc::XFERTYP,
                            4,
                            pending_xfer.take().unwrap(),
                        )
                        .unwrap();
                    assert_eq!(
                        board
                            .esdhc
                            .card_mut()
                            .data_for(18, argument, record.data.len())
                            .unwrap()
                            .unwrap(),
                        record.data,
                        "CMD25 write overlay did not match oracle's input bytes"
                    );
                    input_bytes = record.data.len();
                    writes_issued += 1;
                }
            }
            trace::HWR => {
                let source = reader
                    .sources
                    .get(&record.u16(0))
                    .map(String::as_str)
                    .unwrap_or("");
                if source == "emu.esdhc.Esdhc._dma_out" && record.data.len() >= 512 {
                    let command = pending_reads
                        .pop_front()
                        .expect("DMA output without an armed CMD8/CMD18");
                    if command == 18 {
                        let addr = record.u32(1);
                        for (i, &byte) in record.data.iter().enumerate() {
                            assert_eq!(
                                board.read8(addr + i as u32).unwrap(),
                                byte,
                                "CMD18 after overlay, addr={addr:#x}+{i} clock {}",
                                record.clock
                            );
                        }
                        reads_confirmed += 1;
                    }
                }
                if input_bytes > 0 && source == "emu.esdhc.Esdhc._dma_in" {
                    let addr = record.u32(1);
                    for (i, &expected) in record.data.iter().enumerate() {
                        assert_eq!(
                            board.read8(addr + i as u32).unwrap(),
                            expected,
                            "CMD25 TCD writeback addr={addr:#x}+{i}"
                        );
                    }
                    tcd_writes_checked += 1;
                    if tcd_writes_checked == 3 {
                        writes_confirmed += 1;
                        tcd_writes_checked = 0;
                        if !full {
                            assert!(
                                pending_reads.is_empty(),
                                "stopped with unread CMD8/CMD18 output"
                            );
                            break;
                        }
                    }
                }
            }
            trace::END => {
                clean_end = true;
            }
            _ => {}
        }
    }
    assert!(input_bytes >= 512, "CMD25 guest RAM source not found");
    assert_eq!(
        tcd_writes_checked, 0,
        "CMD25 must write SADDR, CITER and CSR"
    );
    assert!(reads_confirmed >= 2);
    assert!(writes_confirmed >= 1);
    if full {
        assert!(clean_end);
        assert!(pending_xfer.is_none());
        assert!(
            pending_reads.is_empty(),
            "unread CMD8/CMD18 output at trace end"
        );
        assert_eq!(reads_confirmed, issued_cmd18, "each CMD18 must move data");
        assert_eq!(writes_issued, writes_confirmed);
        eprintln!(
            "{trace_path}: {reads_confirmed} CMD18 payloads and {writes_confirmed} CMD25 transfers checked"
        );
    }
}

#[test]
fn armed_read_command_filters_inert_and_write_direction_cmd18() {
    let transfer = (18 << 24) | (1 << 21) | (1 << 4);
    assert_eq!(
        armed_read_command(esdhc::BASE + esdhc::XFERTYP, 4, transfer, false),
        None
    );
    assert_eq!(
        armed_read_command(esdhc::BASE + esdhc::XFERTYP, 4, transfer & !(1 << 4), true),
        None
    );
    assert_eq!(
        armed_read_command(
            esdhc::BASE + esdhc::XFERTYP,
            4,
            (8 << 24) | (1 << 21) | (1 << 4),
            true,
        ),
        Some(8)
    );
}

#[test]
#[ignore = "requires local ignored DT2/DN2 trace and card image environment"]
fn first_late_cmd18_payloads_match_oracle_on_both_products() {
    gate(
        &std::env::var("DT2_LATE_TRACE").expect("set DT2_LATE_TRACE"),
        Some(&std::env::var("DT2_CARD_IMAGE").expect("set DT2_CARD_IMAGE")),
        DEFAULT_CAPACITY_BLOCKS,
        false,
    );
    gate(
        &std::env::var("DN2_LATE_TRACE").expect("set DN2_LATE_TRACE"),
        None,
        SMALL_CAPACITY_BLOCKS,
        false,
    );
}

#[test]
#[ignore = "requires local ignored DT2/DN2 trace and card image environment"]
fn all_late_cmd18_payloads_before_first_write_match_oracle() {
    gate(
        &std::env::var("DT2_LATE_TRACE").expect("set DT2_LATE_TRACE"),
        Some(&std::env::var("DT2_CARD_IMAGE").expect("set DT2_CARD_IMAGE")),
        DEFAULT_CAPACITY_BLOCKS,
        true,
    );
    gate(
        &std::env::var("DN2_LATE_TRACE").expect("set DN2_LATE_TRACE"),
        None,
        SMALL_CAPACITY_BLOCKS,
        true,
    );
}

#[test]
#[ignore = "requires local ignored DT2/DN2 trace and card image environment"]
fn first_late_cmd25_overlay_and_tcd_match_oracle_on_both_products() {
    cmd25_gate(
        &std::env::var("DT2_LATE_TRACE").expect("set DT2_LATE_TRACE"),
        Some(&std::env::var("DT2_CARD_IMAGE").expect("set DT2_CARD_IMAGE")),
        DEFAULT_CAPACITY_BLOCKS,
        false,
    );
    cmd25_gate(
        &std::env::var("DN2_LATE_TRACE").expect("set DN2_LATE_TRACE"),
        None,
        SMALL_CAPACITY_BLOCKS,
        false,
    );
}

#[test]
#[ignore = "requires local ignored DT2/DN2 trace and card image environment"]
fn complete_bounded_late_card_dma_payloads_match_oracle() {
    cmd25_gate(
        &std::env::var("DT2_LATE_TRACE").expect("set DT2_LATE_TRACE"),
        Some(&std::env::var("DT2_CARD_IMAGE").expect("set DT2_CARD_IMAGE")),
        DEFAULT_CAPACITY_BLOCKS,
        true,
    );
    cmd25_gate(
        &std::env::var("DN2_LATE_TRACE").expect("set DN2_LATE_TRACE"),
        None,
        SMALL_CAPACITY_BLOCKS,
        true,
    );
}
