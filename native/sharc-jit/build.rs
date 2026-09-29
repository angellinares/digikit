// SHARC_JIT_RT names the runtime module (./rt built for wasm32); it is
// embedded so the library needs nothing else at run time. It holds no
// firmware (the transpiled core only).
fn main() {
    println!("cargo:rustc-check-cfg=cfg(jit_rt)");
    println!("cargo:rerun-if-env-changed=SHARC_JIT_RT");
    if let Ok(p) = std::env::var("SHARC_JIT_RT") {
        let p = std::path::Path::new(&p).canonicalize().expect("SHARC_JIT_RT");
        println!("cargo:rerun-if-changed={}", p.display());
        println!("cargo:rustc-env=SHARC_JIT_RT={}", p.display());
        println!("cargo:rustc-cfg=jit_rt");
    }
}
