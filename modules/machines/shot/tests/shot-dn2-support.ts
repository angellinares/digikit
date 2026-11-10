import { readFileSync } from 'node:fs';
import path from 'node:path';
import { Sample, prepareUpload, type Prepared } from '@digi/dk-session/dksample';
import { openSamples } from '@digi/dk-session/samples';
import { repoRoot } from '@digi/test-support';
import { expect } from 'vitest';
import { liveArtifacts, record } from '@digi/regression/runtime/artifacts.ts';
import { installWithDebug } from '@digi/regression/runtime/install.ts';
import { memory, panel } from '@digi/regression/runtime/panel.ts';
import { runtimeTests } from '@digi/regression/runtime/ready.ts';
import { planSymbol, prepareRuntimePlan } from '@digi/regression/runtime/runtime-plan.ts';
import { RATE, selectDynamicMachine, trackApi, trackRecord, wav } from './shot-support.ts';

// Shared setup of the DN2 SHOT device tests (shot-dn2-pool, shot-dn2-audio, shot-dn2-supersede): install, plan, the fixture
// files in the digikit store, the pool and DSP readers, and the phase runner. Each test file boots on its own and uploads only
// the files it uses. Phases do not stop at the first failure; the failures are collected and asserted at the end, so one run
// reports every phase of its file.

export const FOLDER = 'shotdn2';
export const PAGE = 4096;
export const FRAMES_PER_PAGE = 2048; // one page of one plane
export const planePages = (frames: number) => Math.ceil(frames / FRAMES_PER_PAGE);
export const physOf = (nw32: number) => 0x80000000 + (nw32 - 0x10000000) * 4;

export interface Fixture {
  name: string;
  prep: Prepared;
  channels: number;
  frames: number;
}

/** The plane of a native file in each byte layout a 4-byte group (two PCM16 frames, big endian in the file: b0 b1 b2 b3) can take in DDR, zero padded
 * to whole pages: `swap` (b1 b0 b3 b2) is the packed little-endian word of finding 87 / 92 (low half = the first frame). */
export const LAYOUTS: Record<string, number[]> = { swap: [1, 0, 3, 2], raw: [0, 1, 2, 3], wordrev: [3, 2, 1, 0], halfswap: [2, 3, 0, 1] };
export function plane(f: Fixture, channel: number, layout: number[]): Buffer {
  const pcm = Buffer.alloc(planePages(f.frames) * PAGE);
  for (let k = 0; k < f.frames; k++) {
    const at = 64 + (k * f.channels + channel) * 2;
    pcm[2 * k] = f.prep.stored[at]!;
    pcm[2 * k + 1] = f.prep.stored[at + 1]!;
  }
  const out = Buffer.alloc(pcm.length);
  for (let q = 0; q < pcm.length; q += 4) for (let j = 0; j < 4; j++) out[q + j] = pcm[q + layout[j]!]!;
  return out;
}

// Name -> [WAV bytes, channels]; a test file lists the names it uses.
const SPECS: Record<string, () => [Uint8Array, number]> = {
  a: () => [wav(3, [440, 660]), 2], // the pool and the stereo checks
  m: () => [wav(2, [330]), 1], // mono
  s: () => [wav(4, [440, 660]), 2], // pan
  r: () => [wav(3.6, (n) => (n < 1.2 * RATE ? [440, 660] : [880, 1320]), (n) => (n < 2.4 * RATE ? 1 : 0)), 2], // tones A for 1.2 s, then B for 1.2 s: a retrigger shows as A again
  x: () => [wav(1.5, [550, 770]), 2], // the file the track moves to
  t: () => [wav(5, [440, 660]), 2], // the long tone and the throughput load
  l: () => [wav(4, [220, 330]), 2], // the load that is superseded
  y: () => [wav(1.2, [600, 900]), 2], // the file that supersedes it
};

export type Dn2 = Awaited<ReturnType<typeof setup>>;

async function setup(runtime: Parameters<Parameters<typeof runtimeTests>[1]>[0], id: string, uploads: string[], extraModules: string[]) {
  const { session, dk } = runtime;
  const artifacts = liveArtifacts(session, id);
  const api = trackApi(runtime);
  // The daemon answers every peek on the emulation thread; a tight poll loop starves the guest (stall faults), so polls are 60 ms apart.
  const settle = (track: number, timeoutMs = 120_000, each?: Parameters<typeof api.settle>[2]) => api.settle(track, timeoutMs, each, 60);
  const mem = memory(session);
  const ui = panel(session);
  const log: Record<string, unknown> = {};
  const failures: string[] = [];
  /** Run one phase; a failure is recorded and the next phase runs. */
  async function phase<T>(name: string, body: () => Promise<T>): Promise<T | undefined> {
    const t0 = Date.now();
    try {
      const value = await body();
      log[`${name}.ms`] = Date.now() - t0;
      return value;
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      failures.push(`${name}: ${message}`);
      log[`${name}.error`] = message;
      return undefined;
    } finally {
      // Faults are cumulative: the count after each phase says which phase raised one.
      try {
        log[`${name}.faultsAfter`] = (await session.faults()).length;
      } catch {
        // diagnostic only
      }
    }
  }
  log.stage = 'install';
  await installWithDebug(runtime, 'digikit/shot', 'test/track-select', ...extraModules);
  expect((await dk.modules()).map((m) => m.name)).toEqual(expect.arrayContaining(['digikit/shot', 'test/track-select', ...extraModules]));
  log.stage = 'plan';
  const plan = await prepareRuntimePlan(session, artifacts, [
    'RECORDS',
    '_dv_params',
    '_sp_boot_status',
    '_sp_status',
    '_sp_pages',
    '_dl_beat',
    '_dl_nerr',
    '_gs_ns',
    '_gs_nbad',
    '_gs_cnt',
    '_gs_seen',
    '_gs_health',
    '_gs_nhb',
    '_gs_stamp',
    '_gs_rbuf',
    '_dv_diag',
  ]);
  const sym = (name: string) => planSymbol(plan, name);
  const dspWords = async (name: string, n: number, offsetWords = 0) => {
    const peekAt = sym(name) + 4 * offsetWords;
    const reply = await mem.dsp(peekAt, 4 * n);
    expect(reply.unknown, `${name} readable`).toBe(0);
    const b = Buffer.from(reply.hex, 'hex');
    return Array.from({ length: n }, (_, i) => b.readUInt32LE(i * 4));
  };
  // `_sp_status`, `_sp_error`, `_sp_errcount`, `_sp_pages`, `_sp_last` are consecutive words.
  const pool = async () => {
    const [status, error, errcount, pages, last] = await dspWords('_sp_status', 5);
    return { status: status!, error: error!, errcount: errcount!, pages: pages!, last: last! };
  };
  const link = async () => {
    const [beat] = await dspWords('_dl_beat', 1);
    const [nerr] = await dspWords('_dl_nerr', 1);
    return { beat: beat!, nerr: nerr! };
  };
  const clocks = async () => {
    const st = (await session.client.call('status')) as { elapsed: number; dsp_frames: number };
    return { elapsed: st.elapsed, dspFrames: st.dsp_frames };
  };
  const rec = (track: number) => trackRecord(session, sym('RECORDS'), track);
  /** `length` bytes of DDR at the physical address, in 64 KiB reads. */
  async function ddr(phys: number, length: number): Promise<Buffer> {
    const parts: Buffer[] = [];
    for (let off = 0; off < length; off += 65536) {
      const n = Math.min(65536, length - off);
      const reply = await mem.dsp(phys + off, n);
      expect(reply.unknown, `DDR 0x${(phys + off).toString(16)} fully known`).toBe(0);
      parts.push(Buffer.from(reply.hex, 'hex'));
    }
    return Buffer.concat(parts);
  }

  // ---- fixtures: the files of this test in the digikit store ----------------------------------------------------------------
  const sample = await Sample.create(Uint8Array.from(readFileSync(path.join(repoRoot(), 'apps/web/public/dksample.wasm'))));
  const backend = openSamples('digikit', { session: dk, hashOf: (data) => sample.hash(data) });
  const fixtures: Record<string, Fixture> = {};
  log.stage = 'upload';
  await backend.mkdir(FOLDER);
  const uploadStart = Date.now();
  const perFile: Record<string, number> = {};
  for (const name of uploads) {
    log.stage = `upload ${name}`;
    const t0 = Date.now();
    const [bytes, channels] = SPECS[name]!();
    const prep = prepareUpload(sample, `${name}.wav`, bytes);
    expect(prep.kind).toBe('converted');
    await backend.put(FOLDER, prep);
    expect((await backend.verify(FOLDER, prep)).ok, `${name} verifies on the card`).toBe(true);
    perFile[name] = Date.now() - t0;
    fixtures[name] = { name, prep, channels, frames: (prep.stored.length - 64 - 16) / (2 * channels) };
  }
  log.uploadPerFile = perFile;
  log.uploadMs = Date.now() - uploadStart;
  const file = (name: string) => `${FOLDER}/${name}`;
  log.stage = 'machine';
  await selectDynamicMachine(runtime, artifacts);
  log.stage = 'phases';
  // The uploads into the digikit store stall the main loop (>1 s of wall time without a hit: the stall events are in the fault list from here on).
  // They are not the pool's: the checks below look for faults that are new since this point.
  const baseline = {
    faults: (await session.faults()).length,
    stalls: ((await session.client.status()) as { heartbeat: { stall_count: number } }).heartbeat.stall_count,
  };
  log.baseline = baseline;
  async function expectNoNewFault(where: string) {
    const faults = await session.faults();
    const st = (await session.client.status()) as { heartbeat: { stall_count: number }; cf: { error?: unknown }; audio?: { halted?: unknown } };
    const fresh = faults.slice(baseline.faults).map((e) => JSON.stringify(e));
    expect(fresh, `new faults at ${where}`).toEqual([]);
    expect(st.heartbeat.stall_count, `new stalls at ${where}`).toBe(baseline.stalls);
    expect(st.cf.error, `cf error at ${where}`).toBeFalsy();
    expect(st.audio?.halted, `DSP halt at ${where}`).toBeFalsy();
  }
  /** The end checks of every file: no module fault, no new fault, every phase passed. */
  async function finish() {
    const modules = await dk.modules();
    log.modules = modules.map((m) => ({ name: m.name, faults: m.faults, off: m.guard_off }));
    for (const m of modules) expect(m.faults, `${m.name} faults`).toBe(0);
    await expectNoNewFault('the end');
    expect(failures, `phase failures:\n${failures.join('\n')}`).toEqual([]);
  }
  return { runtime, session, dk, artifacts, api, settle, mem, ui, log, phase, sym, dspWords, pool, link, clocks, rec, ddr, fixtures, file, expectNoNewFault, finish };
}

/** One DN2 SHOT test file: boot, install, upload `uploads`, run `body` (which calls `c.phase`), then the end checks and the measurement record. */
/** `extraModules`: more modules to install (digikit/remote for a file that turns knobs with setSmp). */
export function dn2Tests(title: string, id: string, uploads: string[], body: (c: Dn2) => Promise<void>, extraModules: string[] = []): void {
  runtimeTests(
    title,
    async (runtime) => {
      const { session, dk } = runtime;
      const artifacts = liveArtifacts(session, id);
      let c: Dn2 | undefined;
      try {
        c = await setup(runtime, id, uploads, extraModules);
        await body(c);
        await c.finish();
      } finally {
        const log = c?.log ?? {};
        try {
          // Diagnostic when the body threw outside a phase: the daemon's view of the CF and the faults.
          log.statusAtEnd = await session.client.status();
          log.faultsAtEnd = (await session.faults()).slice(-8);
        } catch (error) {
          log.statusAtEnd = `unavailable: ${error instanceof Error ? error.message : String(error)}`;
        }
        record(artifacts, 'measurements', log);
        console.info(`${id} measurements`, JSON.stringify(log));
        try {
          record(artifacts, 'runtime-log', await dk.readLog());
        } catch {
          // The log is a diagnostic only.
        }
      }
    },
    { dsp: 'sharc', devices: ['dn2'], patchSet: 'sample-tracks' },
  );
}
