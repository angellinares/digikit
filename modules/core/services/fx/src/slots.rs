//! `fx` service, slots and hooks: none here. The service fills no pointer slot. Its nine hooks are entered directly at their stock sites and are assembler in the
//! service file (`dks/src/hooks.rs`); the state they use is in `fxpage.rs` (the `DKRT_FXP_*` statics).
