//! Firmware-agnostic native board seam: sparse guest RAM, MMIO, and eMMC DMA59.

pub mod board;

pub use board::{Board, BoardWriteError, CompletionEvent, CompletionPolicy, SemaphoreAddresses};

pub mod runner;
pub use runner::Machine;
