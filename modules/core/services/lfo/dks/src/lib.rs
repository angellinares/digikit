//! The `lfo` service as a service file (`services/lfo.dks`): the crate `dkrt-svc-lfo` over the `dkc_*` imports (docs/plans/runtime-and-services.md section 13.2).
#![no_std]

use dkrt_sdk::records::KINDS_LFO;
use dkrt_svc_lfo::records::Lfo;

dkrt_svc_rt::service!(Lfo, "lfo", &KINDS_LFO);
