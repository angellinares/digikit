// The generated-code crate. Generated code lives under out/ (the block code is firmware-derived),
// never under native/. SHARC_GEN_DIR names a directory written by
// tools/sharc_transpile.py (syms.rs, tables.rs, core_i.rs, core_g.rs) and
// optionally tools/sharc_rsgen.py (image.rs and the block files it
// includes). Without it the crate builds the runtime only.
use std::env;
use std::path::Path;

fn main() {
    println!("cargo:rustc-check-cfg=cfg(sharc_gen)");
    println!("cargo:rustc-check-cfg=cfg(sharc_image)");
    println!("cargo:rustc-check-cfg=cfg(sharc_gen_version)");
    println!("cargo:rerun-if-env-changed=SHARC_GEN_DIR");
    let Ok(dir) = env::var("SHARC_GEN_DIR") else {
        return;
    };
    let dir = Path::new(&dir);
    for name in ["syms.rs", "tables.rs", "core_i.rs", "core_g.rs"] {
        if !dir.join(name).is_file() {
            panic!("SHARC_GEN_DIR={} has no {name}", dir.display());
        }
    }
    let dir = dir.canonicalize().expect("SHARC_GEN_DIR");
    println!("cargo:rustc-cfg=sharc_gen");
    if dir.join("image.rs").is_file() {
        println!("cargo:rustc-cfg=sharc_image");
    }
    // Generated before the staleness guard: no GENERATOR_VERSION, reported
    // as version 0 (build_info), which the loaders refuse.
    let tables = std::fs::read_to_string(dir.join("tables.rs")).expect("tables.rs");
    if tables.contains("pub const GENERATOR_VERSION") {
        println!("cargo:rustc-cfg=sharc_gen_version");
    }
    println!("cargo:rustc-env=SHARC_GEN_DIR={}", dir.display());
    println!("cargo:rerun-if-changed={}", dir.display());
    for entry in std::fs::read_dir(&dir).expect("read SHARC_GEN_DIR") {
        let path = entry.expect("dir entry").path();
        if path.extension().is_some_and(|e| e == "rs") {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
}
