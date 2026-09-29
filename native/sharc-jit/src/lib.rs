//! SHARC+ JIT (plan phase P2) and the P1 measurement tools.
//!
//! - [`jit`]: the desktop JIT. The runtime module (`rt/`, the machine
//!   state and the one-instruction interpreter, wasm32) runs under wasmtime;
//!   when it reports a hot PC, the translator (`translate/`) partially
//!   evaluates the transpiled core over the image's instructions into a
//!   region module that shares the runtime's memory and table.
//! - [`capi`]: the native core's C ABI over the JIT (tools/sharc_diff.py,
//!   tools/sharc_transpile_run.py load it like libsharc_native).
//! - `bin/jit_frames.rs`: a frame pack through the JIT, timed and checked.
//! - P1: [`wasmscan`], [`split`], [`host`], `bin/wasm_frames.rs`,
//!   `bin/jit_probe.rs`.

pub mod capi;
pub mod host;
pub mod jit;
pub mod split;
pub mod wasmscan;

#[path = "../../sharc/src/sha256.rs"]
#[allow(dead_code)]
pub mod sha;

/// The transpiled core's hash and generator version (from its tables.rs,
/// the translator's input), as sharc_native_info reports them.
pub fn info() -> String {
    let t = sharc_translate::sources::TABLES;
    let core = t
        .split("pub const CORE_SHA256: &str = \"")
        .nth(1)
        .and_then(|x| x.split('"').next())
        .unwrap_or("none");
    let gen_v = t
        .split("pub const GENERATOR_VERSION: u32 = ")
        .nth(1)
        .and_then(|x| x.split(';').next())
        .and_then(|x| x.trim().parse::<u32>().ok())
        .unwrap_or(0);
    format!(
        "{{\"core_sha256\": \"{core}\", \"generator_version\": {gen_v}, \"image_sha256\": \"none\", \"blocks\": 0, \"jit\": true}}"
    )
}

/// The JIT's counters as JSON.
pub fn stats_json(m: &jit::Machine) -> String {
    let s = &m.store.data().stats;
    let mut fails: Vec<(&String, &u64)> = s.failures.iter().collect();
    fails.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    let fails: Vec<String> = fails
        .iter()
        .take(12)
        .map(|(k, v)| format!("[{:?}, {}]", k, v))
        .collect();
    let sha: String = s.modules_sha.iter().map(|b| format!("{b:02x}")).collect();
    format!(
        "{{\"regions\": {}, \"refused\": {}, \"insns\": {}, \"blocks\": {}, \"bytes\": {}, \"translate_ms\": {:.1}, \"compile_ms\": {:.1}, \"modules_sha256\": \"{}\", \"failures\": [{}]}}",
        s.regions,
        s.refused,
        s.insns,
        s.blocks,
        s.bytes,
        s.translate_ns as f64 / 1e6,
        s.compile_ns as f64 / 1e6,
        sha,
        fails.join(", ")
    )
}
