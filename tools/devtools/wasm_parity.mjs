#!/usr/bin/env node
// Headless parity check of the coupled (ColdFire + SHARC+) browser core.
//
// Loads apps/web/.local/emulator-core-sharc.wasm, restores the DN2 ready session,
// plays a scenario from tests/scenarios/dn2.json exactly as `coupled-run`
// does, and prints the SHA-256 of the PCM (f32le bytes of every value produced
// since the ready point, the same bytes PcmTap hashes). With a scenario that has
// expect_sha256 the exit code is 1 on a mismatch.
//
//   node tools/devtools/wasm_parity.mjs --scenario trig1-tap-release
//   node tools/devtools/wasm_parity.mjs --scenario exactness-100m --out wasm.f32
//
// Options (defaults match tools/emulator/digi_env.py; DIGI_EMU_* variables are honoured):
//   --core PATH        apps/web/.local/emulator-core-sharc.wasm
//   --syx PATH         firmware/Digitone_II_OS1.11.syx            (DIGI_EMU_SYX)
//   (READY = ready_dir of tools/emulator/ready_inputs.json, or DIGI_EMU_READY_DIR)
//   --dsp-image PATH   READY/digi-audio-dn2-image.bin (DIGI_EMU_DSP_IMAGE)
//   --dsp-state PATH   READY/digi-audio-m5.snap.dsp   (DIGI_EMU_DSP_STATE)
//   --cf-snapshot PATH READY/digi-audio-m5.snap       (DIGI_EMU_CF_SNAPSHOT)
//   --scenarios PATH   tests/scenarios/dn2.json
//   --scenario NAME    scenario to play (required)
//   --fast T,M,F       turn the DSP fast tier on (wasm kernels; needs a `fast-wasm` core): threshold, min insns, follow;
//                      `desktop` = the shipped regions and cluster roots of config/emulator.json, `desktop+auto` adds run-time selection
//   --split 1          two core instances in two threads (ColdFire here, DSP in a worker_threads worker); PCM must equal the single instance's
//   --seconds S        override the scenario length in audio seconds (like coupled-run)
//   --out PATH         write the raw f32le PCM (compare with coupled-run --wav x.f32)
//   --leds 1           print every panel LED change the snapshots report (`leds`, led_revision), with the scenario's button events
//   --repo PATH        repo root (default: the parent of this script's directory, else cwd)
import fs from 'node:fs';
import path from 'node:path';
import crypto from 'node:crypto';
import { fileURLToPath } from 'node:url';

const STEP_CHUNK = 250_000; // packages/emulator/coupled STEP_CHUNK
const FRAMES_PER_SECOND = 1500;
const VALUES_PER_FRAME = 64; // 32 stereo samples per DSPI2 frame

const args = {};
for (let i = 2; i < process.argv.length; i++) {
  const a = process.argv[i];
  if (!a.startsWith('--')) throw new Error(`unexpected argument ${a}`);
  args[a.slice(2)] = process.argv[++i];
}
const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(
  args.repo ??
    (fs.existsSync(path.join(here, '..', '..', 'packages', 'emulator')) ? path.join(here, '..', '..') : process.cwd()),
);
const env = process.env;
const ready = path.resolve(
  root,
  env.DIGI_EMU_READY_DIR ??
    JSON.parse(fs.readFileSync(path.join(root, 'tools/emulator/ready_inputs.json'), 'utf8')).ready_dir,
);
const paths = {
  core: args.core ?? path.join(root, 'apps/web/.local/emulator-core-sharc.wasm'),
  syx: args.syx ?? env.DIGI_EMU_SYX ?? env.DIGI_COUPLED_SYX ?? path.join(root, 'firmware', 'Digitone_II_OS1.11.syx'),
  image: args['dsp-image'] ?? env.DIGI_EMU_DSP_IMAGE ?? path.join(ready, 'digi-audio-dn2-image.bin'),
  dsp: args['dsp-state'] ?? env.DIGI_EMU_DSP_STATE ?? path.join(ready, 'digi-audio-m5.snap.dsp'),
  snap: args['cf-snapshot'] ?? env.DIGI_EMU_CF_SNAPSHOT ?? path.join(ready, 'digi-audio-m5.snap'),
  scenarios: args.scenarios ?? path.join(root, 'tests/scenarios/dn2.json'),
};
if (!args.scenario) throw new Error('--scenario NAME is required');
const scenario = JSON.parse(fs.readFileSync(paths.scenarios, 'utf8')).scenarios.find((s) => s.name === args.scenario);
if (!scenario) throw new Error(`no scenario ${args.scenario} in ${paths.scenarios}`);

// The fast tier of the browser core (`--fast threshold,min_insns,follow`, a core
// built with the `fast-wasm` feature): its kernels are wasm modules the core
// emits and this host compiles over the core's own memory.
// `--fast desktop[+auto]`: the desktop configuration (config/emulator.json for this firmware's SHA-256), with run-time selection on top for `+auto`.
const fastConfig = (() => {
  if (!args.fast) return undefined;
  if (!args.fast.startsWith('desktop')) return args.fast.split(',').map(Number).concat([0, 0, 0]).slice(0, 3);
  const table = JSON.parse(fs.readFileSync(path.join(root, 'config/emulator.json'), 'utf8'));
  const entry = table[crypto.createHash('sha256').update(fs.readFileSync(paths.syx)).digest('hex')];
  if (!entry) throw new Error('no entry in config/emulator.json for this firmware');
  const pcs = (t) =>
    (t ?? '')
      .split(',')
      .filter((x) => x.trim())
      .map((x) => parseInt(x, 16));
  return {
    regions: pcs(entry.fast_regions),
    roots: pcs(entry.blocktrans),
    ...(args.fast.startsWith('desktop+auto') ? { auto: [256, 64] } : {}),
  };
})();
const kernels = [];
const hostStats = { compileMs: 0, compiled: 0, runs: 0, bytes: 0 };
const hostEnv = {
  host_wasm_compile(ptr, len) {
    const bytes = new Uint8Array(mem(), ptr, len).slice();
    const t = performance.now();
    try {
      const inst = new WebAssembly.Instance(new WebAssembly.Module(bytes), {
        env: { memory: e.memory, helper: e.digi_helper_exec },
      });
      kernels.push(inst.exports.k0);
    } catch (err) {
      console.error(`kernel module refused: ${err}`);
      return -1;
    }
    hostStats.compileMs += performance.now() - t;
    hostStats.compiled++;
    hostStats.bytes += len;
    return kernels.length - 1;
  },
  host_wasm_run(id, ctx, st) {
    hostStats.runs++;
    return kernels[id](ctx, st);
  },
  host_wasm_free(id) {
    kernels[id] = null;
  },
};
// --split 1: the two-worker session of the browser (apps/web/src/split-session.ts):
// the ColdFire instance runs in this thread, the DSP instance in a worker_threads
// worker (tools/devtools/wasm_split.mjs), DSPI2 frames cross a SharedArrayBuffer. Same
// scenario loop and events, so the PCM must be the single-instance PCM.
let e, mem, call, first, loadS, cfSide, dspDone, linkBuffer;
const dspiStub = {
  host_dspi_exchange() {
    throw new Error('no remote DSP');
  },
  host_dspi_link() {
    throw new Error('no remote DSP');
  },
};
if (args.split === '1') {
  const web = path.join(root, 'apps/web/src');
  const { createCfSide } = await import(path.join(web, 'split-session.ts'));
  const { createLinkBuffer } = await import(path.join(web, 'sab-link.ts'));
  const { Worker } = await import('node:worker_threads');
  const read = (p) => new Uint8Array(fs.readFileSync(p));
  const wasm = read(paths.core);
  linkBuffer = createLinkBuffer();
  const dspWorker = new Worker(path.join(here, 'wasm_split.mjs'), {
    workerData: {
      wasm,
      image: read(paths.image),
      state: read(paths.dsp),
      link: linkBuffer,
      keepPcm: true,
      fast: fastConfig,
    },
    execArgv: process.execArgv,
  });
  dspDone = new Promise((resolve, reject) => {
    dspWorker.on('message', (m) => {
      if (m.type === 'done') resolve(m);
    });
    dspWorker.on('error', reject);
  });
  await new Promise((resolve, reject) => {
    dspWorker.once('message', (m) => m.type === 'ready' && resolve());
    dspWorker.once('error', reject);
  });
  const t0 = performance.now();
  cfSide = await createCfSide({ wasm, syx: read(paths.syx), snapshot: read(paths.snap), link: linkBuffer });
  loadS = (performance.now() - t0) / 1000;
  e = cfSide.core.exports;
  mem = cfSide.core.memory;
  call = cfSide.core.call;
  first = cfSide.loaded;
} else {
  const { instance } = await WebAssembly.instantiate(fs.readFileSync(paths.core), { env: { ...dspiStub, ...hostEnv } });
  e = instance.exports;
  mem = () => e.memory.buffer;
  const json = () =>
    JSON.parse(new TextDecoder().decode(new Uint8Array(mem(), e.digi_result_ptr(), e.digi_result_len())));
  call = (name, ...a) => {
    const c = e[name](...a);
    const v = json();
    if (c !== 0 || v.error) throw new Error(`${name}: ${v.error ?? c}`);
    return v.snapshot ?? v;
  };
  const put = (b) => {
    const p = e.digi_alloc(b.length);
    if (!p) throw new Error('digi_alloc failed');
    new Uint8Array(mem(), p, b.length).set(b);
    return p;
  };

  const bufs = Object.fromEntries(['syx', 'image', 'dsp', 'snap'].map((k) => [k, fs.readFileSync(paths[k])]));
  const ptrs = Object.fromEntries(Object.entries(bufs).map(([k, b]) => [k, put(b)]));
  const t0 = performance.now();
  first = call(
    'digi_load_coupled',
    ptrs.syx,
    bufs.syx.length,
    ptrs.image,
    bufs.image.length,
    ptrs.dsp,
    bufs.dsp.length,
    ptrs.snap,
    bufs.snap.length,
  );
  loadS = (performance.now() - t0) / 1000;
  for (const k of Object.keys(ptrs)) e.digi_dealloc(ptrs[k], bufs[k].length);
}
if (!first.status.ready) throw new Error('the restored snapshot is not at ready');
if (args.split !== '1' && fastConfig) {
  if (Array.isArray(fastConfig)) call('digi_sharc_fast', ...fastConfig);
  else {
    const putU32 = (list) => {
      const p = e.digi_alloc(list.length * 4);
      new Uint32Array(mem(), p, list.length).set(list);
      return p;
    };
    const reg = putU32(fastConfig.regions);
    const roots = putU32(fastConfig.roots);
    call(
      'digi_sharc_fast_regions',
      reg,
      fastConfig.regions.length,
      roots,
      fastConfig.roots.length,
      ...(fastConfig.auto ?? [0xffffffff, 0]),
      0,
    );
  }
}
const readyAt = first.status.icount;
console.error(`scenario ${scenario.name}: load ${loadS.toFixed(2)} s, ready at icount ${readyAt}`);

// The QA prelude of packages/emulator/boot/examples/common (Script::poll): NO down at
// ready, up at +1M, down at +20M, up at +21M, encoder A +1 at +40M. It is
// polled after each chunk, before the scenario's own events of the next loop.
const qa = scenario.qa_script ? { stage: 1 } : null;
if (qa) call('digi_button', 12, 1);
const qaPoll = (icount) => {
  if (!qa) return;
  const elapsed = icount - readyAt;
  for (const [stage, when, act] of [
    [1, 1e6, 0],
    [2, 20e6, 1],
    [3, 21e6, 0],
    [4, 40e6, null],
  ]) {
    if (qa.stage === stage && elapsed >= when) {
      if (act === null) call('digi_turn', 1, 1);
      else call('digi_button', 12, act);
      qa.stage = stage + 1;
    }
  }
};
qaPoll(readyAt);

// at_s events count audio seconds as DSPI2 frames sent / 1500.
const sortKey = (ev) => (ev.at_instr !== undefined ? [0, ev.at_instr] : [1, Math.round(ev.at_s * FRAMES_PER_SECOND)]);
const events = [...(scenario.events ?? [])].sort((a, b) => {
  const [ka, na] = sortKey(a),
    [kb, nb] = sortKey(b);
  return ka - kb || na - nb;
});
const seconds = args.seconds !== undefined ? Number(args.seconds) : scenario.seconds;
const targetFrames = seconds === undefined ? null : Math.ceil(seconds * FRAMES_PER_SECOND);
const untilInstr = args.seconds !== undefined ? null : (scenario.instructions ?? null);

const chunks = [];
let ledState;
let values = 0,
  icount = readyAt,
  next = 0,
  nChunks = 0;
const frames = () => (cfSide ? cfSide.stats().frames : Math.floor(values / VALUES_PER_FRAME));
const wallStart = performance.now();
let lastLog = wallStart;
for (;;) {
  while (next < events.length) {
    const ev = events[next];
    const due =
      ev.at_instr !== undefined ? icount - readyAt >= ev.at_instr : frames() >= Math.round(ev.at_s * FRAMES_PER_SECOND);
    if (!due) break;
    if (args.leds) console.log(`+${((icount - readyAt) / 1e6).toFixed(1)}M event ${JSON.stringify(ev)}`);
    if (ev.button !== undefined) call('digi_button', ev.button, (ev.down ?? true) ? 1 : 0);
    else call('digi_turn', ev.turn, ev.detents ?? 1);
    next++;
  }
  const elapsed = icount - readyAt;
  if (untilInstr !== null ? elapsed >= untilInstr : frames() >= targetFrames) break;
  let budget = STEP_CHUNK;
  if (untilInstr !== null && scenario.clip_last_chunk) budget = Math.min(untilInstr - elapsed, STEP_CHUNK);
  const ev = events[next];
  if (scenario.exact_events && ev && ev.at_instr !== undefined && ev.at_instr > elapsed)
    budget = Math.min(budget, ev.at_instr - elapsed);
  const snap = call('digi_step', budget);
  const status = snap.status;
  if (args.leds && snap.leds !== undefined) {
    const prev = ledState ?? '0'.repeat(128),
      changes = [];
    for (let i = 0; i < 64; i++)
      if (snap.leds.slice(i * 2, i * 2 + 2) !== prev.slice(i * 2, i * 2 + 2))
        changes.push(
          `${i.toString(16).padStart(2, '0')}:${prev.slice(i * 2, i * 2 + 2)}->${snap.leds.slice(i * 2, i * 2 + 2)}`,
        );
    ledState = snap.leds;
    console.log(
      `+${((status.icount - readyAt) / 1e6).toFixed(1)}M leds rev=${status.led_revision} ${changes.join(' ')}`,
    );
  }
  nChunks++;
  icount = status.icount;
  const n = cfSide ? 0 : e.digi_pcm_take(); // split: the PCM is in the DSP worker
  if (n) {
    chunks.push(new Uint8Array(mem(), e.digi_pcm_ptr(), n * 4).slice());
    values += n;
  }
  if (status.error) throw new Error(`step error: ${status.error}`);
  qaPoll(icount);
  if (performance.now() - lastLog > 30000) {
    lastLog = performance.now();
    console.error(
      `  +${((icount - readyAt) / 1e6).toFixed(0)}M cf, ${(values / 2 / 48000).toFixed(2)} s audio, wall ${((lastLog - wallStart) / 1000).toFixed(0)} s`,
    );
  }
}
const wallS = (performance.now() - wallStart) / 1000;
let pcm = Buffer.concat(chunks);
let stats;
if (cfSide) {
  const { LinkServer } = await import(path.join(root, 'apps/web/src/sab-link.ts'));
  new LinkServer(linkBuffer).close();
  const done = await dspDone;
  pcm = Buffer.from(done.pcm);
  values = pcm.length / 4;
  stats = done.stats.engine;
  const s = cfSide.stats();
  console.log(
    `split: ColdFire thread blocked on the DSP ${(s.waitedMs / 1000).toFixed(2)} s of ${wallS.toFixed(2)} s, DSP worker busy ${(done.stats.busyMs / 1000).toFixed(2)} s, ${s.requests} link requests`,
  );
} else stats = call('digi_sharc_stats');
const audioS = values / 2 / 48000;
const sha = crypto.createHash('sha256').update(pcm).digest('hex');
console.log(
  `scenario=${scenario.name} cf_instr=${icount - readyAt} chunks=${nChunks} frames=${stats.frames} dsp_instr=${stats.dsp_instructions} halted=${stats.halted} missing_blocks=${stats.missing_blocks}`,
);
console.log(`pcm values=${values} audio=${audioS.toFixed(3)}s sha256=${sha}`);
console.log(
  `wall=${wallS.toFixed(1)}s load=${loadS.toFixed(2)}s real_time_factor=${(wallS / audioS).toFixed(1)}x slower than real time`,
);
if (args.fast) {
  const k = cfSide
    ? (await dspDone).stats.kernels
    : {
        compiled: hostStats.compiled,
        compileMs: hostStats.compileMs,
        wasmBytes: hostStats.bytes,
        runs: hostStats.runs,
      };
  console.log('fast report', JSON.stringify(cfSide ? (await dspDone).stats.fast : call('digi_sharc_fast_report')));
  console.log(
    `fast tier: ${k.compiled} kernels compiled in ${k.compileMs.toFixed(0)} ms (${(k.wasmBytes / 1e6).toFixed(2)} MB wasm), ${k.runs} kernel runs`,
  );
}
if (args.out) fs.writeFileSync(args.out, pcm);
let rc = 0;
if (scenario.expect_sha256) {
  const ok = scenario.expect_sha256 === sha;
  console.log(`expect_sha256=${scenario.expect_sha256} ${ok ? 'MATCH' : 'MISMATCH'}`);
  rc = ok ? 0 : 1;
}
process.exit(rc);
