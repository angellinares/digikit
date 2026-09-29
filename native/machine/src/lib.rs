//! Firmware-agnostic native board seam: sparse guest RAM, MMIO, and eMMC DMA59.

pub mod board;
pub mod state;
pub mod time;

pub use board::{Board, BoardWriteError, CompletionEvent, CompletionPolicy, SemaphoreAddresses};
pub use state::{MachineState, StateError};
pub use time::{Time, TimeError, TimerPolicy};

pub mod runner;
pub use runner::{Machine, StateApplyError, TimedStepError};
