//! `__NAME__` service, handlers (interface B): what the core calls on the service. Every method has a default that does nothing, so the service
//! overrides only what it owns (`dkrt_abi::service::Service`).
use dkrt_abi::core_api::CoreApi;
use dkrt_abi::service::{Declare, Service};

/// The __NAME__ service.
pub struct __CAMEL__;

impl<C: CoreApi> Service<C> for __CAMEL__ {
    /// The record kinds the service owns: one `d.kind(KIND_X, LEN_X)` for each `[records.kinds]` entry of sdk/sdk.toml with `service = "__NAME__"`
    /// (`patchkit service-check` compares the two).
    unsafe fn declare(_d: &mut Declare) {}
}
