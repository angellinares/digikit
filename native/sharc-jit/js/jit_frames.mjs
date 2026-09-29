// V8 half of `jit-probe run`: compile the block modules `jit-probe split`
// wrote, one at a time (per-part latency) and all together, instantiate
// them against the runtime, install their entries in its table, and replay
// the pack.
//
//   node [V8 flags] jit_frames.mjs SPLITDIR PACK [--repeat R] [--check H]
//                                  [--in-module]
//
// V8 compiles lazily by default (a function is compiled on its first
// call), so compile latency is only meaningful with
// --no-wasm-lazy-compilation, and --liftoff-only (baseline) or
// --no-liftoff (optimising, TurboFan) selects the tier.
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { parseHashes, runPack, summary } from "./frames.mjs";

const args = process.argv.slice(2);
const opt = (name, dflt) => {
  const i = args.indexOf(name);
  return i >= 0 ? args[i + 1] : dflt;
};
const [dir, packPath] = args;
const repeat = Number(opt("--repeat", "5"));
const check = opt("--check", null);
const inModule = args.includes("--in-module");
const hr = () => Number(process.hrtime.bigint());

const parts = readFileSync(join(dir, "manifest.tsv"), "utf8")
  .trim()
  .split("\n")
  .map((l) => {
    const [file, , size, ents] = l.split("\t");
    return {
      bytes: readFileSync(join(dir, file)),
      size: Number(size),
      entries: ents.split(",").map((e) => e.split(":")),
    };
  });

// V8 caches compiled modules by their bytes, so each process measures one
// way: --async compiles the whole set concurrently (V8's worker threads),
// otherwise one part at a time on this thread.
const kb = parts.reduce((s, p) => s + p.size, 0) / 1024;
let modules;
if (args.includes("--async")) {
  const t1 = hr();
  modules = await Promise.all(parts.map((p) => WebAssembly.compile(p.bytes)));
  console.log(`V8 async compile of all ${parts.length} parts: ${((hr() - t1) / 1e6).toFixed(0)} ms wall (${kb.toFixed(0)} KB)`);
} else {
  const lat = [];
  const t0 = hr();
  modules = parts.map((p) => {
    const t = hr();
    const m = new WebAssembly.Module(p.bytes);
    lat.push((hr() - t) / 1e3);
    return m;
  });
  const serialMs = (hr() - t0) / 1e6;
  const sorted = [...lat].sort((a, b) => a - b);
  const q = (f) => sorted[Math.round((sorted.length - 1) * f)];
  console.log(
    `V8 compile per part: median ${q(0.5).toFixed(0)} us, p90 ${q(0.9).toFixed(0)}, max ${q(1).toFixed(0)}; ` +
      `all ${parts.length} parts ${serialMs.toFixed(0)} ms (${kb.toFixed(0)} KB)`,
  );
}

const rtBytes = readFileSync(join(dir, "runtime.wasm"));
const t2 = hr();
const rtModule = new WebAssembly.Module(rtBytes);
const rtCompileMs = (hr() - t2) / 1e6;
const rt = new WebAssembly.Instance(rtModule, { host: { now_ns: hr } });
const env = rt.exports;
const t3 = hr();
let installed = 0;
if (!inModule) {
  modules.forEach((m, i) => {
    const inst = new WebAssembly.Instance(m, { env });
    for (const [slot, name] of parts[i].entries) {
      env.t0.set(Number(slot), inst.exports[name]);
      installed++;
    }
  });
}
console.log(
  `runtime compile ${rtCompileMs.toFixed(0)} ms; instantiate and install ${installed} entries: ${((hr() - t3) / 1e6).toFixed(1)} ms`,
);
const r = runPack(env, readFileSync(packPath), {
  repeat,
  expect: check ? parseHashes(readFileSync(check, "utf8")) : null,
});
console.log(summary(inModule ? "blocks in the runtime module" : "blocks in their own modules", r, repeat));
if (r.differing) process.exit(1);
