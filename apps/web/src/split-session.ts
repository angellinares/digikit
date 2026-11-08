// The two halves of a two-worker coupled session, independent of the worker
// API (browser module workers in dsp-worker.ts / cf-worker.ts, Node
// worker_threads in tools/devtools/wasm_split.mjs):
//
//   ColdFire worker --digi_step--> DSPI2 frame --SharedArrayBuffer--> DSP worker
//   (core instance 1)                                                 (core instance 2)
//
// Each worker has its own instance of the same generic core (no firmware in
// it); the DSP worker's fast tier runs wasm kernels compiled from the loaded
// image at run time (core-host.ts). Only erasable TypeScript.

import { instantiateCore, type Core } from './core-host.ts';
import { LinkClient, LinkServer, MAX_PAYLOAD } from './sab-link.ts';

export interface DspSideOptions {
  wasm: BufferSource | Response | Promise<Response> | WebAssembly.Module;
  /** Packed DSP image and state (`digi-audio-*-image.bin`, `.snap.dsp`): built from the user's .syx. */
  image: Uint8Array;
  state: Uint8Array;
  link: SharedArrayBuffer;
  /** Fast tier: run-time selection `[threshold, min insns, follow]`, or the desktop configuration (omit for AOT and interpreter alone). */
  fast?: [number, number, number] | FastRegions | FastDefault;
  /** Interleaved L/R f32 produced since the last call, about every `pcmEvery` frames. */
  onPcm?: (pcm: Float32Array) => void;
  pcmEvery?: number;
  /** Called about every `statsEvery` frames with the engine's statistics (a worker posts them to the page). */
  onStats?: (stats: DspStats) => void;
  statsEvery?: number;
}

/** Static fast-tier regions and cluster roots (config/emulator.json), with optional run-time selection on top. */
export interface FastRegions {
  regions: number[];
  roots: number[];
  auto?: [number, number];
  follow?: boolean;
}

/** No `?fast=` query: the DSP worker picks `4,2,1` for a generic core (no AOT blocks), else `desktop` if present, else off. */
export interface FastDefault {
  kind: 'default';
  desktop?: FastRegions;
}

export interface DspStats {
  kernels?: Core['kernels'];
  /** The fast tier's report (`digi_sharc_fast_report`), refreshed about every 20 stats messages. */
  fast?: unknown;
  frames: number;
  busyMs: number;
  /** `digi_sharc_stats` of the DSP instance (frames, dsp_instructions, halted, ...). */
  engine: Record<string, unknown>;
  pcmValues: number;
}

export interface DspSide {
  core: Core;
  /** Answers frames until the link closes. */
  serve(): void;
  stats(): DspStats & { kernels: Core['kernels']; fast?: unknown };
}

export async function createDspSide(o: DspSideOptions): Promise<DspSide> {
  const core = await instantiateCore(o.wasm);
  const img = core.put(o.image);
  const st = core.put(o.state);
  core.call('digi_dsp_open', img, o.image.length, st, o.state.length);
  core.free(img, o.image.length);
  core.free(st, o.state.length);
  let fastArmed = false;
  const armRegions = (f: FastRegions) => {
    const regions = core.put(new Uint8Array(new Uint32Array(f.regions).buffer));
    const roots = core.put(new Uint8Array(new Uint32Array(f.roots).buffer));
    const [threshold, minInsns] = f.auto ?? [0xffffffff, 0];
    core.call(
      'digi_sharc_fast_regions',
      regions,
      f.regions.length,
      roots,
      f.roots.length,
      threshold,
      minInsns,
      f.follow ? 1 : 0,
    );
    core.free(regions, f.regions.length * 4);
    core.free(roots, f.roots.length * 4);
    fastArmed = true;
  };
  if (Array.isArray(o.fast)) {
    core.call('digi_sharc_fast', ...o.fast);
    fastArmed = true;
  } else if (o.fast && 'kind' in o.fast) {
    const aot = core.exports.digi_sharc_aot_blocks?.();
    if (aot === 0) {
      core.call('digi_sharc_fast', 4, 2, 1);
      fastArmed = true;
    } else if (o.fast.desktop) armRegions(o.fast.desktop);
  } else if (o.fast) armRegions(o.fast);
  const txp = core.put(new Uint8Array(MAX_PAYLOAD));
  const rxp = core.put(new Uint8Array(MAX_PAYLOAD));
  const server = new LinkServer(o.link);
  let frames = 0;
  const every = o.pcmEvery ?? 4;
  const statsEvery = o.statsEvery ?? 150;
  let pcmValues = 0;
  const flush = () => {
    const n = core.exports.digi_pcm_take() as number;
    if (!n) return;
    pcmValues += n;
    if (o.onPcm) o.onPcm(new Float32Array(core.memory(), core.exports.digi_pcm_ptr() as number, n).slice());
  };
  let lastFast: unknown;
  let statsCalls = 0;
  const stats = (): DspStats => {
    if (fastArmed && statsCalls++ % 20 === 0) lastFast = core.call('digi_sharc_fast_report');
    return {
      frames,
      busyMs: server.busyMs,
      engine: core.call('digi_sharc_stats'),
      pcmValues,
      kernels: { ...core.kernels },
      fast: lastFast,
    };
  };
  return {
    core,
    serve() {
      server.serve(
        // The reply goes out first; the period is rendered after the ColdFire side has been released.
        (tx) => {
          new Uint8Array(core.memory(), txp, tx.length).set(tx);
          if (core.exports.digi_dsp_reply(txp, tx.length, rxp) !== 0) throw new Error('digi_dsp_reply failed');
          return new Uint8Array(core.memory(), rxp, tx.length);
        },
        (bytes) => {
          new Uint8Array(core.memory(), txp, bytes.length).set(bytes);
          core.exports.digi_dsp_link(txp, bytes.length);
        },
        () => {
          core.exports.digi_dsp_render();
          frames++;
          if (frames % every === 0) flush();
          if (o.onStats && frames % statsEvery === 0) o.onStats(stats());
        },
      );
      core.exports.digi_dsp_render();
      flush();
    },
    stats() {
      return {
        ...stats(),
        kernels: core.kernels,
        fast: fastArmed ? core.call('digi_sharc_fast_report') : undefined,
      };
    },
  };
}

export interface CfSideOptions {
  wasm: BufferSource | Response | Promise<Response> | WebAssembly.Module;
  syx: Uint8Array;
  /** ColdFire snapshot at ready (the user's own, from the ready-state cache); empty: cold boot. */
  snapshot: Uint8Array;
  link: SharedArrayBuffer;
}

export interface CfSide {
  core: Core;
  /** The snapshot `digi_load_coupled_remote` returned. */
  loaded: { status: { ready: boolean; icount: number } };
  /** One `digi_step(budget)`; returns the status of the snapshot. */
  step(budget?: number): { icount: number; error?: string };
  /** The latest panel LED hex string (see `RuntimeSnapshot.leds`), or undefined before the first change. */
  leds(): string | undefined;
  stats(): { steps: number; stepMs: number; waitedMs: number; requests: number; frames: number };
}

export async function createCfSide(o: CfSideOptions): Promise<CfSide> {
  const client = new LinkClient(o.link);
  let core: Core;
  core = await instantiateCore(o.wasm, {
    host_dspi_exchange(tx: number, len: number, rx: number) {
      const reply = client.exchange(new Uint8Array(core.memory(), tx, len).slice());
      new Uint8Array(core.memory(), rx, len).set(reply);
    },
    host_dspi_link(ptr: number, len: number) {
      client.link(new Uint8Array(core.memory(), ptr, len).slice());
    },
  });
  const syx = core.put(o.syx);
  const snap = core.put(o.snapshot);
  const loaded = core.call('digi_load_coupled_remote', syx, o.syx.length, snap, o.snapshot.length);
  core.free(syx, o.syx.length);
  core.free(snap, o.snapshot.length);
  let steps = 0;
  let stepMs = 0;
  let lastLeds: string | undefined;
  return {
    core,
    leds: () => lastLeds,
    loaded,
    step(budget = 250_000) {
      const t = performance.now();
      const snapshot = core.call('digi_step', budget);
      if (snapshot.leds !== undefined) lastLeds = snapshot.leds;
      const status = snapshot.status;
      stepMs += performance.now() - t;
      steps++;
      return status;
    },
    stats() {
      return { steps, stepMs, waitedMs: client.waitedMs, requests: client.requests, frames: client.frames };
    },
  };
}
