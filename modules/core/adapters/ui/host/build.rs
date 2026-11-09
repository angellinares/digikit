// Compiles tests/c/*.c against sdk/include into a static library that the tests link.
use std::path::PathBuf;

fn main() {
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    // The menu of core/digikit-menu is compiled by tests/c/menu_fake.c (it includes the source), with the runtime's header.
    let includes = [
        root.join("../../../../../sdk/include"),
        root.join("../../../runtime/include"),
        root.join("../include"),
    ];
    let menu_dir = root.join("../../../apps/digikit-menu/src");
    let src_dir = root.join("tests/c");
    // The pure functions of the sample picker (sdk/src/picker_logic.c) are linked; its screens (sample_picker.c) are included by tests/c/picker_fake.c.
    let sdk_src = root.join("../../../../../sdk/src");
    println!("cargo:rerun-if-changed={}", sdk_src.display());
    for dir in includes.iter().chain([&menu_dir, &src_dir]) {
        println!("cargo:rerun-if-changed={}", dir.display());
    }

    let mut sources: Vec<PathBuf> = std::fs::read_dir(&src_dir)
        .expect("tests/c")
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "c"))
        .collect();
    sources.sort();
    let mut build = cc::Build::new();
    build
        .std("c11")
        .warnings(false)
        .flag("-Wall")
        .flag("-Wextra")
        .flag("-Werror")
        .opt_level(1)
        .includes(&includes)
        .files(&sources)
        .file(sdk_src.join("picker_logic.c"));
    build.compile("dkuisample");
}
