//! The `lfo` service as a service file (`services/lfo.dks`): the crate `dkrt-svc-lfo` over the `dkc_*` imports (docs/plans/runtime-and-services.md section 13.2).
// a host build (the assembly test of the runtime) has std; the file is `no_std` on the ColdFire
#![cfg_attr(target_arch = "m68k", no_std)]

use dkrt_sdk::records::KINDS_LFO;
use dkrt_svc_lfo::records::Lfo;

dkrt_svc_rt::service!(Lfo, "lfo", &KINDS_LFO);
