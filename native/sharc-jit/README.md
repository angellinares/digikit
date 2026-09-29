# sharc-jit: SHARC+ blocks as WebAssembly (P1 measurements)

Phase P1 of `docs/plan-native-emulator.md`: how fast the SHARC+ core runs
as WebAssembly, and what translating blocks at run time costs. Nothing
here embeds firmware; the modules and packs it reads are built under `out/`.

| Path | What |
|---|---|
| `wasm-frames/` | `native/sharc` built for `wasm32-unknown-unknown` with a frame-pack runner (`sw_*` exports, one import `host.now_ns`). |
| `src/bin/wasm_frames.rs` | Runs that module under wasmtime, timed like `sharc-frames`. |
| `js/wasm_frames.mjs` | The same under Node (V8); `js/frames.mjs` is the shared replay loop. |
| `src/wasmscan.rs`, `src/split.rs` | Read a module and split it into a runtime module plus one module per block region (imports memory, table, globals and runtime functions; entries installed in the runtime's table). |
| `src/bin/jit_probe.rs`, `js/jit_frames.mjs` | Compile latency per region and for the hot set, and frame speed with the blocks in their own modules (wasmtime; V8). |
| `js/wasi_run.mjs` | Runs a `wasm32-wasip1` Rust binary under Node's WASI. |

The toolchain needs the `wasm32-unknown-unknown` target (rustup: the
mise-managed 1.98.1 toolchain; `rustup target add wasm32-unknown-unknown`).
Its `rust-lld` may need `DYLD_FALLBACK_LIBRARY_PATH=<toolchain>/lib`.

```sh
G=out/native/opt/gen-final        # any tools/sharc_rsgen.py output
SHARC_GEN_DIR=$PWD/$G CARGO_TARGET_DIR=out/native/p1/target-wasm \
  RUSTFLAGS="-C link-arg=-zstack-size=8388608 -C target-feature=+simd128" \
  cargo build --release --target wasm32-unknown-unknown --manifest-path native/sharc-jit/wasm-frames/Cargo.toml
W=out/native/p1/target-wasm/wasm32-unknown-unknown/release/sharc_wasm_frames.wasm
CARGO_TARGET_DIR=out/native/p1/target-jit cargo build --release --manifest-path native/sharc-jit/Cargo.toml
out/native/p1/target-jit/release/wasm-frames $W out/native/opt/drive3-f0.pack --repeat 5 --check out/native/opt/ref.hashes
node native/sharc-jit/js/wasm_frames.mjs $W out/native/opt/drive3-f0.pack --repeat 10 --check out/native/opt/ref.hashes
out/native/p1/target-jit/release/jit-probe split $W out/native/p1/split
out/native/p1/target-jit/release/jit-probe run out/native/p1/split out/native/opt/drive3-f0.pack --check out/native/opt/ref.hashes
node [--no-wasm-lazy-compilation --no-liftoff] native/sharc-jit/js/jit_frames.mjs out/native/p1/split out/native/opt/drive3-f0.pack
```
