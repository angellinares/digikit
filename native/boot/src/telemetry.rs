//! Bounded observations. No clocks, MMIO reads, printing or guest mutation.
use crate::ExecutionPolicy;
use serde::Serialize;

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Position {
    pub icount: u64,
    pub oracle_ticks: Option<u64>,
    pub interpreted_instructions: u64,
    pub idle_fast_forwarded_instructions: u64,
    pub pc: u32,
}
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Mark {
    Entry,
    TaskCreated,
    IntroPixel,
    IntroRaster,
    IntroPublished,
    IntroDone,
    DisplayStart,
    MainPublished,
    FilesystemStart,
    FilesystemComplete,
    Ready,
    InputReady,
    DspiExchange,
    UartTx,
    Fault,
}
const MARKS: usize = 15;
#[derive(Clone, Copy, Debug, Serialize)]
pub struct Milestone {
    pub(crate) kind: Mark,
    pub at: Position,
    pub boundary: &'static str,
}
#[derive(Debug, Serialize)]
pub struct DiagnosticReport {
    pub schema_version: u32,
    pub device: String,
    pub version: String,
    pub execution_policy: ExecutionPolicy,
    pub main_sha256: String,
    pub at: Position,
    pub milestones: Vec<Milestone>,
    pub interrupt_deliveries: Vec<u64>,
    pub uart_tx_bytes: u64,
    pub dma_written_ranges: u64,
    pub dma_written_bytes: u64,
    pub storage_completions: u64,
    pub dspi_exchanges: u64,
    pub dspi_tx_bytes: u64,
    pub sharc_execution_connected: bool,
    pub pcm_output_connected: bool,
    pub bus_trace_enabled: bool,
    pub fault: Option<String>,
    pub profile: Option<ProfileReport>,
    pub events: Option<EventReport>,
}
#[derive(Debug, Serialize)]
pub struct ProfileReport {
    pub interval_interpreted_instructions: u64,
    pub bucket_bytes: u32,
    pub samples: Vec<PcBucket>,
}
#[derive(Debug, Serialize)]
pub struct PcBucket {
    pub phase: &'static str,
    pub address: u32,
    pub samples: u64,
}
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(
    not(feature = "diagnostic-events"),
    allow(dead_code, reason = "event recording is compiled out")
)]
pub(crate) enum EventKind {
    Milestone,
    TimerDeliveries,
    Input,
    Encoder,
    InputIrq,
    StorageCompletion,
    DmaWrite,
    UartTx,
    DspiExchange,
}
#[derive(Clone, Copy, Debug, Serialize)]
pub struct Event {
    pub sequence: u64,
    pub at: Position,
    pub(crate) kind: EventKind,
    pub value: u64,
}
#[derive(Debug, Serialize)]
pub struct EventReport {
    pub capacity: usize,
    pub dropped: u64,
    pub entries: Vec<Event>,
}

pub(crate) struct Recorder {
    marks: [Option<Milestone>; MARKS],
    #[cfg(feature = "diagnostic-profile")]
    next_sample: u64,
    #[cfg(feature = "diagnostic-profile")]
    bins: Box<[[u64; 1024]; 3]>,
    #[cfg(feature = "diagnostic-events")]
    ring: std::collections::VecDeque<Event>,
    #[cfg(feature = "diagnostic-events")]
    sequence: u64,
}
impl Default for Recorder {
    fn default() -> Self {
        Self {
            marks: [None; MARKS],
            #[cfg(feature = "diagnostic-profile")]
            next_sample: 0,
            #[cfg(feature = "diagnostic-profile")]
            bins: Box::new([[0; 1024]; 3]),
            #[cfg(feature = "diagnostic-events")]
            ring: std::collections::VecDeque::with_capacity(256),
            #[cfg(feature = "diagnostic-events")]
            sequence: 0,
        }
    }
}
impl Recorder {
    pub fn has(&self, kind: Mark) -> bool {
        self.marks[kind as usize].is_some()
    }
    pub fn mark(&mut self, kind: Mark, at: Position, boundary: &'static str) {
        if !self.has(kind) {
            self.marks[kind as usize] = Some(Milestone { kind, at, boundary });
            #[cfg(feature = "diagnostic-events")]
            self.event(EventKind::Milestone, kind as u64, at);
        }
    }
    pub fn milestones(&self) -> Vec<Milestone> {
        let mut marks: Vec<_> = self.marks.iter().flatten().copied().collect();
        marks.sort_by_key(|m| m.at.oracle_ticks.unwrap_or(m.at.icount));
        marks
    }
    #[cfg(feature = "diagnostic-profile")]
    pub fn sample(&mut self, interpreted: u64, pc: u32, phase: usize) {
        if interpreted < self.next_sample {
            return;
        }
        self.next_sample = interpreted.saturating_add(16_381);
        if let Some(offset) = pc.checked_sub(0x4000_0000)
            && let Some(bin) = self.bins[phase].get_mut((offset / 4096) as usize)
        {
            *bin += 1;
        }
    }
    pub fn profile(&self) -> Option<ProfileReport> {
        #[cfg(feature = "diagnostic-profile")]
        {
            Some(ProfileReport {
                interval_interpreted_instructions: 16_381,
                bucket_bytes: 4096,
                samples: self
                    .bins
                    .iter()
                    .enumerate()
                    .flat_map(|(phase, bins)| {
                        bins.iter().enumerate().filter_map(move |(i, &samples)| {
                            (samples != 0).then_some(PcBucket {
                                phase: ["before_intro", "intro", "main"][phase],
                                address: 0x4000_0000 + i as u32 * 4096,
                                samples,
                            })
                        })
                    })
                    .collect(),
            })
        }
        #[cfg(not(feature = "diagnostic-profile"))]
        {
            None
        }
    }
    #[cfg(feature = "diagnostic-events")]
    pub fn event(&mut self, kind: EventKind, value: u64, at: Position) {
        if self.ring.len() == 256 {
            self.ring.pop_front();
        }
        self.ring.push_back(Event {
            sequence: self.sequence,
            at,
            kind,
            value,
        });
        self.sequence += 1;
    }
    pub fn events(&self) -> Option<EventReport> {
        #[cfg(feature = "diagnostic-events")]
        {
            Some(EventReport {
                capacity: 256,
                dropped: self.sequence.saturating_sub(256),
                entries: self.ring.iter().copied().collect(),
            })
        }
        #[cfg(not(feature = "diagnostic-events"))]
        {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn position(n: u64) -> Position {
        Position {
            icount: n,
            oracle_ticks: Some(n),
            interpreted_instructions: n,
            idle_fast_forwarded_instructions: 0,
            pc: 0x4000_04e8,
        }
    }
    #[test]
    fn milestones_retain_first_observation_and_export_in_time_order() {
        let mut r = Recorder::default();
        r.mark(Mark::Ready, position(200), "chunk_end");
        r.mark(Mark::Entry, position(0), "before_instruction");
        r.mark(Mark::Ready, position(300), "chunk_end");
        let marks = r.milestones();
        assert_eq!(marks.len(), 2);
        assert_eq!(marks[0].at.icount, 0);
        assert_eq!(marks[1].at.icount, 200);
    }
    #[cfg(feature = "diagnostic-profile")]
    #[test]
    fn sampling_is_bounded_and_counts_interpreted_boundaries_separately_by_phase() {
        let mut r = Recorder::default();
        for n in 0..32_762 {
            r.sample(n, 0x4000_04e8, 0);
        }
        r.sample(32_762, 0x4018_2d7c, 1);
        let p = r.profile().unwrap();
        assert_eq!(p.samples.len(), 2);
        assert_eq!(p.samples[0].samples, 2);
        assert_eq!(p.samples[1].phase, "intro");
        assert_eq!(p.samples[1].samples, 1);
    }
    #[cfg(feature = "diagnostic-events")]
    #[test]
    fn event_ring_retains_latest_entries_and_reports_loss() {
        let mut r = Recorder::default();
        for n in 0..300 {
            r.event(EventKind::TimerDeliveries, n, position(n));
        }
        let events = r.events().unwrap();
        assert_eq!(events.dropped, 44);
        assert_eq!(events.entries.len(), 256);
        assert_eq!(events.entries[0].sequence, 44);
        assert_eq!(events.entries[255].sequence, 299);
    }
}
