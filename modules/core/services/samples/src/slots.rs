//! `samples` service, slots and hooks: none. The service fills no firmware slot and installs no hook. The adapters of the image call it through the
//! C entry points `dkrt_pool_done`, `dkrt_pool_lost` and `dkrt_project_changed` (the runtime's `handlers.rs`).
