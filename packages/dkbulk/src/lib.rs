//! `dkbulk`: the host side of the digikit USB bulk link (docs/plans/usb-fast.md sections 5 and 7).
//!
//! - `proto`: the frames; `gen_bulk` holds the numbers (generated from `sdk/sdk.toml [protocol.bulk]`, `mise run gen:modules`).
//! - `Link`: whole frames in and out. `usb::UsbLink` is the nusb link; `fake::FakeDevice` is the device rules in memory, for tests.
//! - `Client`: INFO, a whole-file PUT with two DATA requests in flight, VERIFY, ABORT.
pub mod client;
pub mod fake;
#[rustfmt::skip]
pub mod gen_bulk;
pub mod link;
pub mod proto;
pub mod usb;

pub use client::{Client, Error, Info, Options};
pub use link::{Link, LinkError};
