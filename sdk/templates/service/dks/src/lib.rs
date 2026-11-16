//! The `__NAME__` service as a service file: the `service!` call and nothing else. The hook assembler is `hooks.rs` and the firmware calls are `fw.rs`, when the service has any.
#![no_std]

use dkrt_sdk::records::KINDS___UPPER__;
use dkrt_svc___NAME__::records::__CAMEL__;

dkrt_svc_rt::service!(__CAMEL__, "__NAME__", &KINDS___UPPER__);
