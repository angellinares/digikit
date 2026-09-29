//! One guest ColdFire instruction and its synchronous board effects.
//!
//! This is not a timer/INTC scheduler. A Device DMA IRQ vector/level must
//! be provided by the host; no firmware-specific vector is assumed here.

use std::collections::BTreeMap;

use coldfire::{Bus, Cpu, InterruptPolicy, Stop};

use crate::{
    Board, CompletionEvent, CompletionPolicy, MachineState,
    host_state::{self, HostStateError},
    state::{MAX_MAPPED_PAGES, PAGE_SIZE},
    time::{TimeError, TimerPolicy},
    timer_state::TimerStateError,
};

/// Why a checkpoint cannot be applied to a fresh native machine.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StateApplyError {
    StatusRegisterOutOfRange { value: u32 },
    ConflictingRambarAliases { c04: u32, c05: u32 },
    UnsupportedComponents,
    InvalidComponents,
    OracleHostStateRequired,
    CardCapacityMismatch { expected: u32, actual: u32 },
    TimerNotAttached,
    TimerState(TimerStateError),
    InvalidMappedInput,
    ForcedMmioOverlapsOwned,
    Ram,
}

/// Why a timed step could not complete at its instruction boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TimedStepError {
    Stop(Stop),
    DeviceTimingUnsupported,
    TimerNotAttached,
    TimerHostWriteUnavailable {
        address: u32,
    },
    VectorOutOfRange {
        vector: u16,
    },
    MissingOracleHandler {
        vector: u8,
    },
    InterruptFrameUnavailable {
        vector: u8,
    },
    /// A preflight passed but CPU exception entry still stopped; CPU state may
    /// have changed and callers must treat this machine as poisoned.
    InterruptDeliveryPoisoned {
        stop: Stop,
    },
}

pub struct Machine {
    pub cpu: Cpu,
    pub board: Board,
    /// Guest instructions elapsed since the imported checkpoint.
    pub clock: u64,
    /// MOVEC selectors the native CPU does not model, retained for export.
    pub unknown_ctlregs: BTreeMap<u32, u32>,
    pub ff1_count: u32,
    pub movec_count: u32,
    dma_irq: Option<(u8, u8)>,
    pending_dma_irq: bool,
    held_events: Vec<CompletionEvent>,
    pub irqs_delivered: u64,
}

impl Machine {
    pub fn new(cpu: Cpu, board: Board) -> Self {
        Self {
            cpu,
            board,
            clock: 0,
            unknown_ctlregs: BTreeMap::new(),
            ff1_count: 0,
            movec_count: 0,
            dma_irq: None,
            pending_dma_irq: false,
            held_events: Vec::new(),
            irqs_delivered: 0,
        }
    }

    /// Apply portable checkpoint state to a newly constructed machine.
    ///
    /// A sole timer component or a validated Python-v1 longrun set with
    /// dormant UART/TX host state can be imported. Nonempty UART/TX queues,
    /// unrepresented host counters and unknown components are rejected.
    /// Failed application leaves this newly built machine unusable; discard it.
    pub fn apply_state(&mut self, state: &MachineState) -> Result<(), StateApplyError> {
        if state.regs.sr > u32::from(u16::MAX) {
            return Err(StateApplyError::StatusRegisterOutOfRange {
                value: state.regs.sr,
            });
        }
        let c04 = state.ctlregs.get(&0xc04).copied();
        let c05 = state.ctlregs.get(&0xc05).copied();
        if let (Some(c04), Some(c05)) = (c04, c05)
            && c04 != c05
        {
            return Err(StateApplyError::ConflictingRambarAliases { c04, c05 });
        }
        if state.mapped_bases.len() > MAX_MAPPED_PAGES
            || state.mapped_bases.windows(2).any(|pair| pair[0] >= pair[1])
            || state
                .mapped_bases
                .iter()
                .any(|base| !(*base as usize).is_multiple_of(PAGE_SIZE))
            || state.pages.len() > state.mapped_bases.len()
            || state
                .pages
                .windows(2)
                .any(|pair| pair[0].base >= pair[1].base)
            || state.pages.iter().any(|page| {
                !(page.base as usize).is_multiple_of(PAGE_SIZE)
                    || page.data.len() != PAGE_SIZE
                    || state.mapped_bases.binary_search(&page.base).is_err()
            })
        {
            return Err(StateApplyError::InvalidMappedInput);
        }
        let (has_timers, host_state) = match &state.components {
            serde_json::Value::Null => (false, None),
            serde_json::Value::Object(components) if components.is_empty() => (false, None),
            serde_json::Value::Object(components)
                if components.len() == 1 && components.contains_key("timers") =>
            {
                (true, None)
            }
            serde_json::Value::Object(components) if components.len() == 4 => {
                let parsed = host_state::parse(&state.components).map_err(|error| match error {
                    HostStateError::Invalid => StateApplyError::InvalidComponents,
                    HostStateError::Unsupported => StateApplyError::UnsupportedComponents,
                })?;
                (true, Some(parsed))
            }
            _ => return Err(StateApplyError::UnsupportedComponents),
        };
        if let Some(host) = &host_state {
            let expected = self.board.esdhc.card_mut().blocks();
            if expected != host.card.blocks {
                return Err(StateApplyError::CardCapacityMismatch {
                    expected,
                    actual: host.card.blocks,
                });
            }
        }
        if has_timers {
            if self.board.completion_policy() != CompletionPolicy::Oracle {
                return Err(StateApplyError::OracleHostStateRequired);
            }
            let time = self
                .board
                .time_mut()
                .ok_or(StateApplyError::TimerNotAttached)?;
            if time.policy() != TimerPolicy::Oracle {
                return Err(StateApplyError::OracleHostStateRequired);
            }
            time.restore_timer_component(state)
                .map_err(StateApplyError::TimerState)?;
        }
        self.board
            .install_forced_mmio(state.mmio_forced.clone())
            .map_err(|_| StateApplyError::ForcedMmioOverlapsOwned)?;

        // Map all bases first: absent page records mean mapped all-zero RAM.
        for &base in &state.mapped_bases {
            self.board
                .map_zeroed_ram_page(base)
                .map_err(|_| StateApplyError::Ram)?;
        }
        for page in &state.pages {
            self.board
                .import_ram_page(page.base, &page.data)
                .map_err(|_| StateApplyError::Ram)?;
        }
        if let Some(host) = &host_state {
            self.board
                .restore_storage_state(host)
                .map_err(|_| StateApplyError::InvalidComponents)?;
        }

        self.cpu.d = state.regs.d;
        self.cpu.a = state.regs.a;
        self.cpu.pc = state.regs.pc;
        self.cpu.sr = state.regs.sr as u16;
        self.unknown_ctlregs.clear();
        for (&selector, &value) in &state.ctlregs {
            match selector {
                0x002 => self.cpu.ctrl.cacr = value,
                0x003 => self.cpu.ctrl.asid = value,
                0x004..=0x007 => self.cpu.ctrl.acr[(selector - 0x004) as usize] = value,
                0x008 => self.cpu.ctrl.mmubar = value,
                0x009 => self.cpu.ctrl.rgpiobar = value,
                0x00c..=0x00f => self.cpu.ctrl.acr[(selector - 0x00c + 4) as usize] = value,
                0x800 => self.cpu.other_a7 = value,
                0x801 => self.cpu.ctrl.vbr = value,
                // Snapshot register fields win over these Python hook keys.
                0x80e | 0x80f => {}
                0xc04 | 0xc05 => self.cpu.ctrl.rambar = value,
                _ => {
                    self.unknown_ctlregs.insert(selector, value);
                }
            }
        }
        self.ff1_count = state.ff1_count;
        self.movec_count = state.movec_count;
        self.clock = state.clock;
        Ok(())
    }

    /// Completion events retained when the most recent step stopped.
    pub fn held_completion_events(&self) -> &[CompletionEvent] {
        &self.held_events
    }

    /// The host supplies an INTC-approved vector and level. None disables
    /// delivery but leaves an already pending request latched.
    pub fn configure_dma_irq(&mut self, irq: Option<(u8, u8)>) -> Result<(), &'static str> {
        if irq.is_some_and(|(_, level)| !(1..=7).contains(&level)) {
            return Err("DMA interrupt level must be 1..=7");
        }
        self.dma_irq = irq;
        Ok(())
    }

    /// Execute at most one guest instruction. Return effects to the caller,
    /// rather than silently discarding data/command completion events.
    pub fn step(&mut self) -> Result<Vec<CompletionEvent>, Stop> {
        if self.pending_dma_irq
            && let Some((vector, level)) = self.dma_irq
            && ((self.cpu.sr >> 8) & 7) < u16::from(level)
            && self.cpu.take_interrupt(
                &mut self.board,
                vector,
                Some(level),
                InterruptPolicy::Device,
            )?
        {
            self.pending_dma_irq = false;
            self.irqs_delivered += 1;
        }
        let result = self.cpu.step(&mut self.board);
        for (addr, len) in self.board.take_dma_written_ranges() {
            self.cpu.invalidate_external_write(addr, len);
        }
        let mut new_events = self.board.take_completion_events();
        for event in &new_events {
            if let CompletionEvent::Dma59 {
                policy: CompletionPolicy::Device,
                completion,
            } = event
                && completion.done
                && completion.major_interrupt
            {
                self.pending_dma_irq = true;
            }
        }
        if let Err(stop) = result {
            self.held_events.append(&mut new_events);
            return Err(stop);
        }
        let mut events = std::mem::take(&mut self.held_events);
        events.append(&mut new_events);
        self.clock += 1;
        Ok(events)
    }

    /// Arm the PIT at the current boundary, execute one instruction, then
    /// atomically offer due Oracle PIT interrupts at the new boundary.
    /// Device timer delivery is explicitly not implemented.
    pub fn step_timed(&mut self) -> Result<Vec<CompletionEvent>, TimedStepError> {
        let mut time = self
            .board
            .take_time()
            .ok_or(TimedStepError::TimerNotAttached)?;
        if time.policy() == TimerPolicy::Device {
            self.board.restore_time(time);
            return Err(TimedStepError::DeviceTimingUnsupported);
        }
        // Arming must happen before the first instruction in this interval:
        // otherwise an enabled PIT starts one instruction late.
        let _ = time.deadline(self.clock);
        self.board.restore_time(time);

        let events = self.step().map_err(TimedStepError::Stop)?;
        let mut time = self
            .board
            .take_time()
            .ok_or(TimedStepError::TimerNotAttached)?;
        time.seed_sr(self.cpu.sr);
        let mut delivery_error = None;
        let service = time.service_with(self.clock, |raw_vector, level| {
            let vector = match u8::try_from(raw_vector) {
                Ok(vector) => vector,
                Err(_) => {
                    delivery_error = Some(TimedStepError::VectorOutOfRange { vector: raw_vector });
                    return false;
                }
            };
            match self
                .board
                .read32(self.cpu.ctrl.vbr.wrapping_add(4 * u32::from(vector)))
            {
                Ok(handler) if handler != 0 && handler < 0x4800_0000 => {}
                _ => {
                    delivery_error = Some(TimedStepError::MissingOracleHandler { vector });
                    return false;
                }
            }
            // `Cpu::exception` sets supervisor SR first, potentially swapping
            // A7 with OTHER_A7 when EUSP is enabled, then stores two words.
            let stack = if self.cpu.sr & 0x2000 == 0 && self.cpu.ctrl.cacr & 0x20 != 0 {
                self.cpu.other_a7
            } else {
                self.cpu.a[7]
            };
            let frame = (stack & !3).wrapping_sub(8);
            if !self.board.can_write_ram_range(frame, 4)
                || !self.board.can_write_ram_range(frame.wrapping_add(4), 4)
            {
                delivery_error = Some(TimedStepError::InterruptFrameUnavailable { vector });
                return false;
            }
            match self.cpu.take_interrupt(
                &mut self.board,
                vector,
                Some(level),
                InterruptPolicy::Oracle,
            ) {
                Ok(true) => true,
                Ok(false) => {
                    delivery_error = Some(TimedStepError::MissingOracleHandler { vector });
                    false
                }
                Err(stop) => {
                    delivery_error = Some(TimedStepError::InterruptDeliveryPoisoned { stop });
                    false
                }
            }
        });
        let host_writes = time.take_host_writes();
        self.board.restore_time(time);
        for write in host_writes {
            self.board
                .apply_timer_host_write(write.addr, write.byte)
                .map_err(|_| TimedStepError::TimerHostWriteUnavailable {
                    address: write.addr,
                })?;
        }
        service.map_err(|TimeError::DeviceInterruptDeliveryUnsupported| {
            TimedStepError::DeviceTimingUnsupported
        })?;
        if let Some(error) = delivery_error {
            return Err(error);
        }
        Ok(events)
    }
}
