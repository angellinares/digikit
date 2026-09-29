// Replay a frame pack through the SHARC+ core built for WebAssembly
// (native/sharc-jit/wasm-frames) under Node (V8), timed like sharc-frames.
//
//   node wasm_frames.mjs MODULE.wasm PACK [--repeat R] [--frames N]
//                        [--check HASHES] [--no-blocks]
//
// --check compares each frame's canonical-state SHA-256 with a
// `sharc-frames --record` file. "first repetition" is the first pass (V8
// starts functions in its baseline compiler and tiers up while running).
import { readFileSync } from "node:fs";
import { parseHashes, runPack, summary } from "./frames.mjs";

const args = process.argv.slice(2);
const opt = (name, dflt) => {
  const i = args.indexOf(name);
  return i >= 0 ? args[i + 1] : dflt;
};
const [wasmPath, packPath] = args;
const repeat = Number(opt("--repeat", "5"));
const check = opt("--check", null);

const hr = () => Number(process.hrtime.bigint());
const t0 = hr();
const bytes = readFileSync(wasmPath);
const module = new WebAssembly.Module(bytes);
const t1 = hr();
const instance = new WebAssembly.Instance(module, { host: { now_ns: hr } });
const r = runPack(instance.exports, readFileSync(packPath), {
  repeat,
  frames: Number(opt("--frames", "Infinity")),
  expect: check ? parseHashes(readFileSync(check, "utf8")) : null,
  blocks: !args.includes("--no-blocks"),
});
console.log(summary(`node ${process.version}`, r, repeat));
console.log(`compile ${((t1 - t0) / 1e6).toFixed(0)} ms (V8 compiles lazily), module ${(bytes.length / 1e6).toFixed(1)} MB`);
if (r.differing) process.exit(1);
