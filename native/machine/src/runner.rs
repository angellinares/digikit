//! One guest ColdFire instruction and its synchronous board effects.
//!
//! This is not a timer/INTC scheduler. A Device DMA IRQ vector/level must
//! be provided by the host; no firmware-specific vector is assumed here.

use coldfire::{Cpu, InterruptPolicy, Stop};

use crate::{Board, CompletionEvent, CompletionPolicy};

pub struct Machine {
    pub cpu: Cpu,
    pub board: Board,
    pub clock: u64,
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
            dma_irq: None,
            pending_dma_irq: false,
            held_events: Vec::new(),
            irqs_delivered: 0,
        }
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
}
