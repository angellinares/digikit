// The timed frame-pack replay over a wasm-frames instance's exports, shared
// by wasm_frames.mjs and jit_frames.mjs (and usable in a browser: pass the
// pack bytes and a clock).
//
// The figure is the median over frames (frame 0 skipped when it is the
// start-up call) of the best of `repeat` runs of the block handler call.

export function runPack(x, pack, { repeat = 5, frames = Infinity, expect = null, blocks = true, log = console.log }) {
  const err = () => {
    const p = x.sw_alloc(4096);
    const n = x.sw_error(p, 4096);
    return new TextDecoder().decode(new Uint8Array(x.memory.buffer, p, n));
  };
  const ptr = x.sw_alloc(pack.length);
  new Uint8Array(x.memory.buffer, ptr, pack.length).set(pack);
  const nAll = x.sw_open(ptr, pack.length);
  if (nAll < 0) throw new Error(err());
  const n = Math.min(nAll, frames);
  x.sw_blocks(blocks ? 1 : 0);
  const first = x.sw_first();
  const hashBuf = x.sw_alloc(32);
  const best = new Array(n).fill(Infinity);
  const firstRep = new Array(n).fill(0);
  const insns = new Array(n).fill(0);
  let bad = 0;
  let total = 0;
  for (let rep = 0; rep < repeat; rep++) {
    if (rep > 0 && x.sw_reload() !== 0) throw new Error(err());
    for (let k = 0; k < n; k++) {
      const c = x.sw_frame(k);
      if (c < 0) throw new Error(err());
      const ns = x.sw_last_ns();
      best[k] = Math.min(best[k], ns);
      if (rep === 0) {
        firstRep[k] = ns;
        insns[k] = c;
        total += c;
        if (expect) {
          x.sw_hash(hashBuf);
          const d = [...new Uint8Array(x.memory.buffer, hashBuf, 32)]
            .map((b) => b.toString(16).padStart(2, "0"))
            .join("");
          if (expect[k] !== d) {
            bad++;
            if (bad <= 5) log(`frame ${first + k}: state differs from the recorded one`);
          }
        }
      }
    }
  }
  const median = (v) => {
    const s = [...v].sort((a, b) => a - b);
    return s[Math.floor(s.length / 2)];
  };
  const skip = n > 1 && first === 0 ? 1 : 0;
  return {
    first,
    frames: n,
    usPerFrame: median(best.slice(skip)) / 1e3,
    nsPerInstr: median(best.slice(skip).map((ns, i) => ns / insns[i + skip])),
    firstRepUs: median(firstRep.slice(skip)) / 1e3,
    instructions: total,
    genericPct: (100 * x.sw_single_steps()) / (total * repeat),
    differing: expect ? bad : null,
    memoryMB: x.memory.buffer.byteLength / 1e6,
  };
}

export function summary(label, r, repeat) {
  let s =
    `${label}: frames ${r.first}-${r.first + r.frames - 1} x${repeat}: median ${r.usPerFrame.toFixed(1)} us/frame, ` +
    `${r.nsPerInstr.toFixed(2)} ns/instr, ${r.instructions} instructions, generic ${r.genericPct.toFixed(2)}%, ` +
    `first repetition ${r.firstRepUs.toFixed(1)} us/frame, memory ${r.memoryMB.toFixed(0)} MB`;
  if (r.differing !== null) s += `\ncheck: ${r.frames - r.differing} of ${r.frames} frames identical`;
  return s;
}

export function parseHashes(text) {
  return text.trim().split("\n").map((l) => l.split(" ")[1]);
}
