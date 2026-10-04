// Generated code lives under out/ (the block code is firmware-derived),
// never under native/, and is compiled by native/sharc-gen. This crate only
// needs to know whether SHARC_GEN_DIR names one, to match sharc-gen's cfgs
// (sharc_gen, sharc_image, sharc_gen_version); the files themselves are not
// tracked here, so editing the engine never depends on them.
use std::env;
use std::path::Path;

fn main() {
    println!("cargo:rustc-check-cfg=cfg(sharc_gen)");
    println!("cargo:rustc-check-cfg=cfg(sharc_image)");
    println!("cargo:rustc-check-cfg=cfg(sharc_gen_version)");
    println!("cargo:rustc-check-cfg=cfg(region_bench_sol)");
    println!("cargo:rerun-if-env-changed=SHARC_GEN_DIR");
    // The region bench's hand-written versions (firmware-derived, never in
    // the repository): compiled in only when this names an existing file.
    println!("cargo:rerun-if-env-changed=SHARC_REGIONBENCH_SOL");
    if let Ok(path) = env::var("SHARC_REGIONBENCH_SOL")
        && Path::new(&path).is_file()
    {
        println!("cargo:rustc-cfg=region_bench_sol");
        println!("cargo:rustc-env=SHARC_REGIONBENCH_SOL_PATH={path}");
        println!("cargo:rerun-if-changed={path}");
    }
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
    // Generated before the staleness guard: no GENERATOR_VERSION.
    let tables = std::fs::read_to_string(dir.join("tables.rs")).expect("tables.rs");
    if tables.contains("pub const GENERATOR_VERSION") {
        println!("cargo:rustc-cfg=sharc_gen_version");
    }
}
