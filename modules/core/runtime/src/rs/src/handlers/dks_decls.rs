//! The declarations of the real `init` of each Rust service file (`dk_svc_declare` of `service!`, the glue of the file's own crate built for the host), for the assembly
//! test that compares them with the packed header table. The runtime names the service file crates only here (layercheck `service-crates`).

/// `dk_svc_declare` of a service file: the declarations of `init` without `setup`, for the device that `device` gives.
pub type DeclareFn = unsafe fn(*mut u8, fn() -> u32) -> i32;

/// The three Rust service files: (file name, declarations).
pub fn declarations() -> [(&'static str, DeclareFn); 3] {
    [
        ("machines", dkrt_svc_machines_dks::dk_svc_declare),
        ("fx", dkrt_svc_fx_dks::dk_svc_declare),
        ("lfo", dkrt_svc_lfo_dks::dk_svc_declare),
    ]
}
