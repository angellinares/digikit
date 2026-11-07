//! Firmware-agnostic native board seam: sparse guest RAM, MMIO, and eMMC DMA59.

pub mod board;
mod host_state;
pub mod state;
pub mod time;
pub mod timer_state;

pub use board::{
    Board, BoardWriteError, CompletionEvent, CompletionPolicy, MIDI_TX_CHAN, MIDI_TX_SOURCE,
    MIDI_TX_VECTOR, MIDI_UDR, SemaphoreAddresses,
};
pub use state::{MachineState, StateError};
pub use time::{DeviceDtimError, DevicePitError, Time, TimeError, TimerPolicy};

pub mod runner;
pub use runner::{Machine, StateApplyError, TimedStepError};
