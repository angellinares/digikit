//! `digi-control`: the host-agnostic emulator controller and the control
//! protocol (`docs/design/emulator-control.md`).
//!
//! - [`proto`]: JSON-RPC messages, error codes, parameter access (no I/O).
//! - [`controller`]: the owner thread of the coupled runtime, the webview
//!   command set, jobs, run modes, `firmware.load` and its injection paths.
//! - [`dispatch`]: one request object in, one response out; any transport
//!   (the Unix socket here, a worker's `postMessage` later) feeds it.
//! - [`server`] (feature `server`, unix): the socket listener and the
//!   discovery file.
//! - [`load`]: the live CPU meters behind `status.load`.
//! - [`firmware`], [`events`], [`cli`]: section/DSP diffs, event fan-out and
//!   the shared command line.

pub mod cardmirror;
pub mod cards;
pub mod cli;
pub mod controller;
pub mod devicefolders;
pub mod dispatch;
pub mod events;
pub mod firmware;
pub mod health;
pub mod load;
pub mod proto;
pub mod runtime_patch;
#[cfg(all(feature = "server", unix))]
pub mod server;

pub use controller::{
    CardSource, Command, Config, Handle, JsonReply, Mode, Reply, ScenarioSetup, load_scenario,
    spawn,
};
pub use devicefolders::DeviceFoldersConfig;
pub use events::Events;
pub use proto::{RpcError, RpcResult, SCHEMA_VERSION};
