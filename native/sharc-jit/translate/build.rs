// The transpiled core is read as source at build time: SHARC_GEN_DIR names a
// tools/sharc_transpile.py output directory (firmware-free).
use std::env;
use std::path::Path;

fn main() {
    println!("cargo:rustc-check-cfg=cfg(sharc_gen)");
    println!("cargo:rerun-if-env-changed=SHARC_GEN_DIR");
    let Ok(dir) = env::var("SHARC_GEN_DIR") else {
        return;
    };
    let dir = Path::new(&dir).canonicalize().expect("SHARC_GEN_DIR");
    for name in ["syms.rs", "tables.rs", "core_g.rs"] {
        let p = dir.join(name);
        assert!(p.is_file(), "SHARC_GEN_DIR={} has no {name}", dir.display());
        println!("cargo:rerun-if-changed={}", p.display());
    }
    println!("cargo:rustc-cfg=sharc_gen");
    println!("cargo:rustc-env=SHARC_GEN_DIR={}", dir.display());
}
