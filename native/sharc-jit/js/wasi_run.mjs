// Run a Rust binary built for wasm32-wasip1 under Node's WASI (V8), with
// the filesystem visible at the same absolute paths.
//
//   node wasi_run.mjs PROGRAM.wasm [ARGS...]
import { readFileSync } from "node:fs";
import { WASI } from "node:wasi";

const [prog, ...rest] = process.argv.slice(2);
const wasi = new WASI({
  version: "preview1",
  args: [prog, ...rest],
  env: process.env,
  preopens: { "/": "/" },
  returnOnExit: true,
});
const module = new WebAssembly.Module(readFileSync(prog));
const instance = new WebAssembly.Instance(module, wasi.getImportObject());
process.exitCode = wasi.start(instance);
