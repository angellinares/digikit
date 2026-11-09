import { readFileSync } from 'node:fs';
import path from 'node:path';
import { Sample, prepareUpload, type Prepared } from '@digi/dk-session/dksample';
import { openSamples } from '@digi/dk-session/samples';
import { button, repoRoot } from '@digi/test-support';
import { expect } from 'vitest';
import { liveArtifacts, record } from '@digi/regression/runtime/artifacts.ts';
import { installWithDebug } from '@digi/regression/runtime/install.ts';
import { memory, panel } from '@digi/regression/runtime/panel.ts';
import { runtimeTests } from '@digi/regression/runtime/ready.ts';
import { settleUi } from '@digi/regression/runtime/waits.ts';
import { planSymbol, prepareRuntimePlan } from '@digi/regression/runtime/runtime-plan.ts';
import {
  AUDIBLE,
  RATE,
  allFinite,
  captureNote,
  expectSplitPair,
  expectedWindow,
  onset,
  peak,
  selectDynamicMachine,
  setSmp,
  tone,
  trackApi,
  trackRecord,
  until,
  wav,
} from './shot-support.ts';

// End to end, DN2 OS 1.12: the sample pool (core/sample-pool: allocator, LP0 page transport into DDR), the runtime sample pool (API 1.5/1.6),
// the dsp-voice stereo hooks (docs/design/dn2-stereo.md) and digikit/shot. The `sample-tracks` patch set of devices/digitone-ii.toml.
//
// Samples are uploaded to the digikit store (`samples/shotdn2/...`) and added to the pool by the root-relative path through test/track-select (handle 1).
// Observed: the runtime's `dk_strack` records and the pool's DSP counters (the firmware's own records), DDR through dsp.peek, and the coupled
// PCM (pcm.tap). The pool picks the entry of a file (the first free one) and SHOT plays the entry its SMP value names (0 by default), so a phase that
// needs a sample on the played track puts it in entry 0 (`api.select(file, 0)` is clear + add); other entries hold loads that are only observed.
// Phases do not stop at the first failure; the failures are collected and asserted at the end, so one run reports every phase.

const FOLDER = 'shotdn2';
const PAGE = 4096;
const FRAMES_PER_PAGE = 2048; // one page of one plane
// [adapter.sample_pool] ddr_base is 0x80531000 in devices/digitone-ii.toml.
const NW32_BASE = 0x1014c400; // sp_nw32(DDR_BASE), docs/design/dn2-sample-pool.md section 1
// Pages per second of emulated DSP time (the ceiling is 1500). A guard against a large fall, not a hardware requirement.
// A page is 2048 mono frames, so a stereo voice streaming at 48 kHz needs about 47 pages/s; 150 is about 3x that.
// Runs measure 187-193 (204 before 2026-11), and blocks vary about 4% between runs, so the rate is not exact.
const THROUGHPUT_FLOOR = 150;
const BLOCKS_PER_SECOND = 1500;

const planePages = (frames: number) => Math.ceil(frames / FRAMES_PER_PAGE);
const physOf = (nw32: number) => 0x80000000 + (nw32 - 0x10000000) * 4;

interface Fixture {
  name: string;
  prep: Prepared;
  channels: number;
  frames: number;
}

runtimeTests(
  'DN2 sample pool, stereo and dsp-link coexistence with SHOT through the runtime sample pool',
  async (runtime) => {
    const { session, dk } = runtime;
    const artifacts = liveArtifacts(session, 'shot-dn2');
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
    try {
      log.stage = 'install';
      await settleUi(session);
      await installWithDebug(runtime, 'digikit/shot', 'test/track-select');
      await settleUi(session);
      expect((await dk.modules()).map((m) => m.name)).toEqual(
        expect.arrayContaining(['digikit/shot', 'test/track-select']),
      );
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
      /** The plane of a native file in each byte layout a 4-byte group (two PCM16 frames, big endian in the file: b0 b1 b2 b3) can take in DDR, zero padded
       * to whole pages: `swap` (b1 b0 b3 b2) is the packed little-endian word of finding 87 / 92 (low half = the first frame). */
      const LAYOUTS: Record<string, number[]> = { swap: [1, 0, 3, 2], raw: [0, 1, 2, 3], wordrev: [3, 2, 1, 0], halfswap: [2, 3, 0, 1] };
      function plane(f: Fixture, channel: number, layout: number[]): Buffer {
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

      // ---- fixtures: six files in the digikit store ---------------------------------------------------------------------------
      const sample = await Sample.create(
        Uint8Array.from(readFileSync(path.join(repoRoot(), 'apps/web/public/dksample.wasm'))),
      );
      const backend = openSamples('digikit', { session: dk, hashOf: (data) => sample.hash(data) });
      const fixtures: Record<string, Fixture> = {};
      const specs: [string, Uint8Array, number][] = [
        ['a', wav(3, [440, 660]), 2], // the pool and the stereo checks
        ['m', wav(2, [330]), 1], // mono
        ['s', wav(4, [440, 660]), 2], // pan
        ['r', wav(3.6, (n) => (n < 1.2 * RATE ? [440, 660] : [880, 1320]), (n) => (n < 2.4 * RATE ? 1 : 0)), 2], // tones A for 1.2 s, then B for 1.2 s: a retrigger shows as A again
        ['x', wav(1.5, [550, 770]), 2], // the file the track moves to
        ['t', wav(5, [440, 660]), 2], // the long tone and the throughput load
        ['l', wav(4, [220, 330]), 2], // the load that is superseded
        ['y', wav(1.2, [600, 900]), 2], // the file that supersedes it
      ];
      log.stage = 'upload';
      await backend.mkdir(FOLDER);
      const uploadStart = Date.now();
      const perFile: Record<string, number> = {};
      for (const [name, bytes, channels] of specs) {
        log.stage = `upload ${name}`;
        const t0 = Date.now();
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
      // The uploads into the digikit store stall the main loop (>1 s of wall time without a hit: the stall events are in the fault list from here on, run 1 and 2).
      // They are not the pool's: the checks below look for faults that are new since this point.
      const baseline = { faults: (await session.faults()).length, stalls: ((await session.client.status()) as { heartbeat: { stall_count: number } }).heartbeat.stall_count };
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

      // ---- 0. boot state of the pool -------------------------------------------------------------------------------------------
      await phase('boot', async () => {
        const [boot] = await dspWords('_sp_boot_status', 1);
        const p = await pool();
        log.boot = { bootStatus: boot, ...p };
        expect(boot, '_sp_boot_status').toBe(1);
        expect(p.status, '_sp_status (armed, idle)').toBe(1);
        expect(p.error, '_sp_error').toBe(0);
      });

      // ---- 1. A (stereo) and M (mono): QUEUED -> READY, handle, DDR planes -------------------------------------------------------
      let aRecord: Awaited<ReturnType<typeof rec>> | undefined;
      async function checkHandle(f: Fixture, track: number, expectedPages: number) {
        const s = await api.status(track);
        const r = await rec(track);
        log[`handle.${f.name}`] = { status: s, record: r };
        expect(r.unknown).toBe(0);
        expect(r.ready, `${f.name} record ready`).toBe(1);
        expect(r.frames).toBe(f.frames);
        expect(r.channels).toBe(f.channels);
        expect(r.rate).toBe(RATE);
        expect(r.token).toBe(s.token);
        expect((r.baseL! - NW32_BASE) % 1024, 'base_l is page aligned (1,024 words)').toBe(0);
        expect(r.baseL!, 'base_l inside the range').toBeGreaterThanOrEqual(NW32_BASE);
        if (f.channels === 2) {
          expect(r.baseR, 'base_r = base_l + one plane').toBe(r.baseL! + planePages(f.frames) * 1024);
        } else {
          expect(r.baseR, 'mono: base_r = base_l').toBe(r.baseL);
        }
        void expectedPages;
        return r;
      }
      const layoutSeen = 'wordrev';
      async function checkPlanes(f: Fixture, r: Awaited<ReturnType<typeof rec>>) {
        const result: Record<string, unknown> = {};
        const planes = (f.channels === 2 ? [[0, r.baseL!], [1, r.baseR!]] : [[0, r.baseL!]]) as [number, number][];
        for (const [channel, base] of planes) {
          const got = await ddr(physOf(base), planePages(f.frames) * PAGE);
          const matches = Object.entries(LAYOUTS)
            .filter(([, perm]) => got.equals(plane(f, channel, perm)))
            .map(([name]) => name);
          const want = plane(f, channel, LAYOUTS[layoutSeen]!);
          let firstDiff = -1;
          for (let i = 0; i < got.length; i++) if (got[i] !== want[i]) { firstDiff = i; break; }
          const pad = got.subarray(f.frames * 2);
          result[`plane${channel}`] = {
            matches,
            firstDiffVsLayout: firstDiff,
            got: got.subarray(Math.max(0, firstDiff - 4), Math.max(0, firstDiff - 4) + 16).toString('hex'),
            want: want.subarray(Math.max(0, firstDiff - 4), Math.max(0, firstDiff - 4) + 16).toString('hex'),
            head: got.subarray(0, 16).toString('hex'),
            padZero: pad.every((b) => b === 0),
            padBytes: pad.length,
          };
          expect(matches.length, `${f.name} plane ${channel} equals the file's plane in some byte layout ${JSON.stringify(result)}`).toBeGreaterThan(0);
          // dsp.peek shows the DDR words of the DSP as the file's big-endian 32-bit words with the bytes reversed (run 1 and 2 of this test); SHOT reads
          // them as a clean tone, so this is the layout the DSP wants.
          expect(matches, `${f.name} plane ${channel}: layout`).toContain(layoutSeen);
          expect(
            pad.every((b) => b === 0),
            `${f.name} plane ${channel}: zero pad after the last frame`,
          ).toBe(true);
        }
        log[`planes.${f.name}`] = result;
      }
      await phase('stereo-load', async () => {
        const f = fixtures.a!;
        expect((await api.status(0)).state, 'track 1 starts unused').toBe(0);
        const before = await pool();
        const sel = await api.select(file('a'), 0);
        expect(sel, 'select queues').toBe(0);
        const ready = await settle(0);
        log.selectA = ready;
        expect(ready.state, `A reaches ready ${JSON.stringify(ready.seen)}`).toBe(3);
        expect(ready.seen.map((s) => s.state)).toEqual(expect.arrayContaining([3]));
        const after = await pool();
        expect(after.pages - before.pages, 'pages sent: both planes').toBe(2 * planePages(f.frames));
        expect(after.errcount - before.errcount, 'no rejected page').toBe(0);
        aRecord = await checkHandle(f, 0, 2 * planePages(f.frames));
        await checkPlanes(f, aRecord);
      });
      await phase('mono-load', async () => {
        const f = fixtures.m!;
        const before = await pool();
        expect(await api.select(file('m'), 1), 'add takes entry 1').toBe(0);
        const ready = await settle(1);
        expect(ready.state, `M reaches ready ${JSON.stringify(ready.seen)}`).toBe(3);
        const after = await pool();
        expect(after.pages - before.pages, 'pages sent: one plane').toBe(planePages(f.frames));
        const r = await checkHandle(f, 1, planePages(f.frames));
        await checkPlanes(f, r);
      });

      // ---- 2. dedup: the same file added again is the same entry ----------------------------------------------------------------
      await phase('dedup', async () => {
        const before = await pool();
        expect(await api.add(file('a')), 'A is already entry 0: add returns it').toBe(0);
        const ready = await settle(0);
        expect(ready.state).toBe(3);
        const after = await pool();
        const r0 = await rec(0);
        log.dedup = { pagesBefore: before.pages, pagesAfter: after.pages, entry0: aRecord, again: r0 };
        expect(after.pages, 'no page for the second add').toBe(before.pages);
        expect(r0.token, 'no new load: the token is the one of the first load').toBe(aRecord!.token);
        expect(r0.baseL, 'same base_l').toBe(aRecord!.baseL);
        expect(r0.baseR, 'same base_r').toBe(aRecord!.baseR);
      });

      // ---- 2b. peak windows deep inside the ready sample A (3 s stereo), one of them larger than one 64 KiB read step ----------------
      await phase('window', async () => {
        const f = fixtures.a!;
        const s = await api.status(0);
        expect(s.state, 'A is ready').toBe(3);
        const small = Math.floor(f.frames * 0.7) + 123;
        const large = Math.floor(f.frames * 0.2);
        const cases: [string, number, number][] = [
          ['small', small, 950],
          ['large', large, Math.min(40000, f.frames - large)],
        ];
        for (const [name, first, frames] of cases) {
          const got = await api.awaitWindow(runtime, 0, s.token, first, frames, 95);
          log[`window.${name}`] = { first, frames, ret: got.ret, ms: got.ms };
          expect(got.ret, `${name} window ready`).toBe(95);
          expect(got.cols, `${name} window columns`).toEqual(expectedWindow(f.prep.stored, f.channels, first, frames, 95));
        }
      });

      // ---- 3. stereo audio: SHOT playing L 440 Hz / R 660 Hz through the stock filter and pan -----------------------------------
      const holdNote = async (name: string, seconds: number) =>
        captureNote(runtime, path.join(artifacts, `${name}.wav`), async (clock) => {
          await until(clock, (await clock()) + seconds * RATE);
        });
      /** The stereo hooks' own counters (core/dsp-voice, dn2-stereo.md): stereo slots stamped in the last block, stamped slots a stage without a twin cleared, the
       * stamps of the 16 slots against the heartbeat, and the peak of each slot's R buffer. */
      const stereoDiag = async () => {
        const rb = await mem.dsp(sym('_gs_rbuf'), 16 * 128);
        const floats = Buffer.from(rb.hex, 'hex');
        const rPeaks = Array.from({ length: 16 }, (_, v) => {
          let m = 0;
          for (let i = 0; i < 32; i++) m = Math.max(m, Math.abs(floats.readFloatLE(v * 128 + i * 4)));
          return Number(m.toFixed(4));
        });
        const [ns] = await dspWords('_gs_ns', 1);
        const [nbad] = await dspWords('_gs_nbad', 1);
        const [nhb] = await dspWords('_gs_nhb', 1);
        const [heartbeat] = await dspWords('_dv_diag', 1);
        const stamps = await dspWords('_gs_stamp', 16);
        const seen = await dspWords('_gs_seen', 16);
        const cnt = await dspWords('_gs_cnt', 32);
        const [health] = await dspWords('_gs_health', 1);
        return { ns, nbad, nhb, heartbeat, stamps: stamps.map((x) => heartbeat! - x), seen: seen.map((x) => heartbeat! - x), cnt, health, rPeaks };
      };
      await phase('stereo-audio', async () => {
        const diag: unknown[] = [];
        const c = await captureNote(runtime, path.join(artifacts, 'note-a.wav'), async (clock) => {
          await until(clock, (await clock()) + 0.8 * RATE);
          diag.push(await stereoDiag());
          await until(clock, (await clock()) + 0.2 * RATE);
          diag.push(await stereoDiag());
          await until(clock, (await clock()) + 0.5 * RATE);
        });
        log.stereoDiag = diag;
        const o = onset(c);
        log.noteA = { onset: o, frames: c.l.length, finite: allFinite(c.l) && allFinite(c.r) };
        expect(o, 'the note is audible').toBeGreaterThanOrEqual(0);
        expect(allFinite(c.l) && allFinite(c.r), 'finite output').toBe(true);
        log.noteASplit = expectSplitPair('A', c, o + 4800, o + RATE, 440, 660);
        // Every stage between the machine and the pan has an R twin (dn2-stereo.md): the guards never refuse on the default track. `_gs_cnt` words 0 to 12 count the
        // refusals of an unsupported context by stage, 16 a bad R, 17 the cap; bit 0 of `_gs_health` is "some stereo slot played R = L in the last block".
        const last = diag[diag.length - 1] as Awaited<ReturnType<typeof stereoDiag>>;
        expect(last.nbad, 'no stamp cleared by a guard').toBe(0);
        expect(last.cnt.slice(0, 18), 'no refusal, no bad R, no cap').toEqual(Array.from({ length: 18 }, () => 0));
        expect(last.health! & 1, 'health: no stereo slot played R = L').toBe(0);
        await expectNoNewFault('first note');
      });

      // ---- 4. retrigger: the fade-in of R restarts --------------------------------------------------------------------------------
      await phase('retrigger', async () => {
        expect(await api.select(file('r'), 0)).toBe(0);
        expect((await settle(0)).state).toBe(3);
        const marks: Record<string, number> = {};
        const c = await captureNote(runtime, path.join(artifacts, 'retrigger.wav'), async (clock) => {
          marks.press = await clock();
          await until(clock, marks.press + 2.3 * RATE);
          await session.client.inputButton(button('TRIG 1'), false);
          await session.client.inputButton(button('TRIG 1'), true);
          marks.retrig = await clock();
          await until(clock, marks.retrig + 1.3 * RATE);
        });
        // The sample plays tones A (440/660) for 1.2 s, then B (880/1320); the sound starts at the key (run 3: A from the press). At the retrigger it is in B (A
        // is only a leak of the stock stages, about half of B); after it A is back at full level within 0.1 s and B has dropped.
        const level = (hz: number[], from: number, to: number) => hz.reduce((m, f) => m + tone(c.l, f, from, to) + tone(c.r, f, from, to), 0);
        const [b0, b1] = [marks.retrig! - 0.45 * RATE, marks.retrig! - 0.1 * RATE];
        const [a0, a1] = [marks.retrig! + 0.6 * RATE, marks.retrig! + 1.2 * RATE];
        const m = {
          marks,
          beforeA: level([440, 660], b0, b1),
          beforeB: level([880, 1320], b0, b1),
          afterA: level([440, 660], a0, a1),
          afterB: level([880, 1320], a0, a1),
          finite: allFinite(c.l) && allFinite(c.r),
        };
        log.retrigger = m;
        const text = JSON.stringify(m);
        expect(m.finite, 'finite output').toBe(true);
        expect(m.beforeB, `before the retrigger the sample is in its B part ${text}`).toBeGreaterThan(Math.max(0.05, m.beforeA * 1.5));
        expect(m.afterA, `after the retrigger the sample starts again with A ${text}`).toBeGreaterThan(Math.max(0.05, m.beforeA * 2.5));
        expect(m.afterB, `B has dropped after the retrigger ${text}`).toBeLessThan(m.beforeB * 0.6);
        await expectNoNewFault('retrigger');
      });
      log.nonTwinnedStage =
        'not arranged: a filter type that dispatches to the non-twinned cases needs a DN2 filter-type edit on the panel; the model test of dn2-stereo.md covers it';

      // ---- 5. release and reselect: no transfer ----------------------------------------------------------------------------------
      await phase('release-reselect', async () => {
        // Entry 0 holds R (the retrigger phase moved it on from A): replace it by X, then by A again; A's block is cached.
        expect(await api.select(file('x'), 0)).toBe(0);
        expect((await settle(0)).state).toBe(3);
        const afterX = await pool();
        const before = await rec(0);
        expect(before.baseL, 'X is another block').not.toBe(aRecord!.baseL);
        expect(await api.select(file('a'), 0)).toBe(0);
        const ready = await settle(0);
        expect(ready.state).toBe(3);
        const afterA = await pool();
        const again = await rec(0);
        log.reselect = { pagesAfterX: afterX.pages, pagesAfterA: afterA.pages, again, first: aRecord };
        expect(afterA.pages, 'the cached block needs no page').toBe(afterX.pages);
        expect(again.baseL, 'the same place').toBe(aRecord!.baseL);
        expect(again.baseR).toBe(aRecord!.baseR);
        expect(again.token, 'a new entry token').not.toBe(aRecord!.token);
        const first = await ddr(physOf(again.baseL!), PAGE);
        expect(first.equals(plane(fixtures.a!, 0, LAYOUTS[layoutSeen]!).subarray(0, PAGE)), 'content unchanged').toBe(true);
      });

      // ---- 6. throughput: a fresh load of T, counters against the DSP block counter -----------------------------------------------
      let tReady = false;
      await phase('throughput', async () => {
        const f = fixtures.t!;
        const p0 = await pool();
        const l0 = await link();
        const c0 = await clocks();
        const samples: { pages: number; beat: number; elapsed: number; dspFrames: number }[] = [];
        expect(await api.select(file('t'), 0)).toBe(0);
        const ready = await settle(0, 120_000, async () => {
          const [p, l, c] = [await pool(), await link(), await clocks()];
          samples.push({ pages: p.pages, beat: l.beat, elapsed: c.elapsed, dspFrames: c.dspFrames });
        });
        const p1 = await pool();
        const l1 = await link();
        const c1 = await clocks();
        expect(ready.state, `T reaches ready ${JSON.stringify(ready.seen)}`).toBe(3);
        tReady = true;
        const pages = p1.pages - p0.pages;
        const first = samples.find((s) => s.pages > p0.pages);
        const rate = (a: { pages: number; beat: number }, b: { pages: number; beat: number }) =>
          (b.pages - a.pages) / Math.max(1e-9, (b.beat - a.beat) / BLOCKS_PER_SECOND);
        const overall = rate({ pages: p0.pages, beat: l0.beat }, { pages: p1.pages, beat: l1.beat });
        const active = first ? rate(first, { pages: p1.pages, beat: l1.beat }) : overall;
        log.throughput = {
          pages,
          expectedPages: 2 * planePages(f.frames),
          blocks: l1.beat - l0.beat,
          dspFrames: c1.dspFrames - c0.dspFrames,
          elapsed: c1.elapsed - c0.elapsed,
          pagesPerSecondOverall: overall,
          pagesPerSecondActive: active,
          polls: samples.length,
          beats: [l0.beat, l1.beat],
          nerr: [l0.nerr, l1.nerr],
          errcount: [p0.errcount, p1.errcount],
        };
        expect(pages, 'pages sent').toBe(2 * planePages(f.frames));
        expect(p1.errcount - p0.errcount, 'no rejected page').toBe(0);
        expect(active, `pages per second (DSP time) ${JSON.stringify(log.throughput)}`).toBeGreaterThanOrEqual(THROUGHPUT_FLOOR);
        // dsp-link coexistence: its block hook kept running during the transfer and counted no link error.
        expect(l1.beat - l0.beat, 'dsp-link heartbeat advanced during the transfer').toBeGreaterThan(100);
        expect(l1.nerr - l0.nerr, 'no dsp-link error during the transfer').toBe(0);
      });

      // ---- 7. supersede: T plays (entry 0), L loads in entry 1 and Y replaces it after ~20 pages -------------------------------
      await phase('supersede', async () => {
        expect(tReady, 'T is loaded').toBe(true);
        const y = fixtures.y!;
        const marks: Record<string, number> = {};
        const info: Record<string, unknown> = {};
        const c = await captureNote(runtime, path.join(artifacts, 'supersede.wav'), async (clock) => {
          marks.press = await clock();
          await until(clock, marks.press + 0.6 * RATE);
          const p0 = await pool();
          const l0 = await link();
          expect(await api.select(file('l'), 1)).toBe(0);
          marks.selectL = await clock();
          // wait for about 20 pages of L, then replace it
          let p = p0;
          const t0 = Date.now();
          while (p.pages - p0.pages < 20) {
            if (Date.now() - t0 > 60_000) throw new Error('L did not start');
            p = await pool();
            await new Promise((r) => setTimeout(r, 40));
          }
          info.pagesBeforeY = p.pages - p0.pages;
          expect(await api.select(file('y'), 1)).toBe(0);
          marks.selectY = await clock();
          const ready = await settle(1);
          marks.readyY = await clock();
          info.readyY = ready;
          const pEnd = await pool();
          const lEnd = await link();
          info.pages = { l: p.pages - p0.pages, total: pEnd.pages - p0.pages, errcount: pEnd.errcount - p0.errcount };
          info.link = { beats: lEnd.beat - l0.beat, nerr: lEnd.nerr - l0.nerr };
          await until(clock, marks.readyY + 0.4 * RATE);
        });
        log.supersede = { marks, ...info };
        const end = marks.readyY!;
        // the tone stays finite and non-silent in 100 ms windows over the whole load window
        const quiet: number[] = [];
        for (let from = marks.press! + 0.4 * RATE; from + 4800 <= end; from += 4800)
          if (Math.max(peak(c.l, from, from + 4800), peak(c.r, from, from + 4800)) < 0.01) quiet.push(from);
        log.supersedeQuietWindows = quiet;
        expect(allFinite(c.l) && allFinite(c.r), 'finite output').toBe(true);
        expect(quiet, 'the tone never goes silent during the supersede').toEqual([]);
        expect(tone(c.l, 440, marks.press! + 0.3 * RATE, end), 'L tone present').toBeGreaterThan(AUDIBLE);
        const stats = info.pages as { l: number; total: number; errcount: number };
        expect((info.readyY as { state: number }).state, 'Y reaches ready').toBe(3);
        // a full L is 2 * 4 s = 188 pages; the superseded load stops long before that
        expect(stats.total, `L was cut short (pages ${JSON.stringify(stats)})`).toBeLessThan(
          2 * planePages(fixtures.l!.frames) * 0.5 + 2 * planePages(y.frames),
        );
        expect(stats.errcount, 'no rejected page').toBe(0);
        expect((info.link as { nerr: number }).nerr, 'no dsp-link error').toBe(0);
        const r1 = await rec(1);
        expect(r1.frames, 'entry 1 holds Y').toBe(y.frames);
        await expectNoNewFault('supersede');
      });

      // ---- 8. a path that is not there ---------------------------------------------------------------------------------------------
      await phase('not-found', async () => {
        const index = await api.add('nothere/zzz');
        log.notFound = { index };
        if (index < 0) return; // refused at once is an acceptable form
        const s = await settle(index, 30_000);
        log.notFound = { index, state: s.state };
        expect(s.state, 'NOT_FOUND').toBe(-1);
        expect(await api.clear(index), 'the failed entry can be cleared').toBe(0);
      });

      // ---- 8b. SMP picks the pool entry of a note: T is entry 0 and L (4 s, 220/330 Hz) is added as entry 2, so entry 1 (Y) lies between them. The page is still
      // SHOT's: knob D is SMP. Both SMP values are turned while a long sample plays, because `_dv_params` is rewritten only while a voice renders.
      await phase('smp', async () => {
        const index = await api.add(file('l'));
        expect(index, 'L takes the first free entry after Y').toBe(2);
        expect((await settle(index)).state, 'L is ready').toBe(3);
        const readSmp = async () => {
          const peek = await mem.dsp(sym('_dv_params'), 4);
          expect(peek.unknown, '_dv_params readable').toBe(0);
          return Buffer.from(peek.hex, 'hex').readUInt32LE(0);
        };
        const note = async (name: string, hzL: number, hzR: number) => {
          const c = await captureNote(runtime, path.join(artifacts, `${name}.wav`), async (clock) => {
            await until(clock, (await clock()) + 1.2 * RATE);
          });
          const o = onset(c);
          expect(o, `${name} is audible`).toBeGreaterThanOrEqual(0);
          return expectSplitPair(name, c, o + 4800, o + RATE, hzL, hzR);
        };
        log.smpTurnToL = await setSmp(runtime, readSmp, index);
        log.smpL = await note('smp-l', 220, 330);
        log.smpTurnToT = await setSmp(runtime, readSmp, 0);
        log.smpT = await note('smp-t', 440, 660);
        await expectNoNewFault('smp');
      });

      // ---- 9. every filter type plays stereo: scan the FLTR page 1 knobs while T plays and read the rate of `_gs_nbad` (stamps a guard of a twin clears). All
      // stages have an R twin, so the rate stays 0 at every position (run 2, before the twins of the shaper and the held sample, showed R = L with `_gs_nbad` rising
      // by one per block on the default track).
      await phase('stereo-config', async () => {
        expect(tReady, 'T is loaded').toBe(true);
        const nbad = async () => (await dspWords('_gs_nbad', 1))[0]!;
        const scan: Record<string, unknown>[] = [];
        const marks: Record<string, number> = {};
        await ui.tap('FLTR');
        const c = await captureNote(runtime, path.join(artifacts, 'stereo-config.wav'), async (clock) => {
          // The track's stock Delay return (default TIME 64: first echo about 1.09 s after the key press, both tones in both channels, then a tail that
          // outlives the voice) adds about -18 dB of the other tone to each channel from then on (run 4: the split window sat at 1.0 to 1.2 s). The audio
          // check is therefore the first row, measured before the echo (key press + 0.45 to 0.75 s); the rows after it only read the guard counter.
          await until(clock, (await clock()) + 0.15 * RATE);
          const rate = async (frames: number) => {
            const [n0, f0] = [await nbad(), await clock()];
            await until(clock, f0 + frames);
            const [n1, f1] = [await nbad(), await clock()];
            return { rate: (n1 - n0) / Math.max(1, f1 - f0) * 32, from: f0, to: f1 };
          };
          scan.push({ enc: 'default', ...(await rate(0.15 * RATE)) });
          for (const enc of ['ENC A', 'ENC B', 'ENC C', 'ENC D', 'ENC E', 'ENC F', 'ENC G', 'ENC H']) {
            for (const dir of [-1, 1]) {
              await ui.burst(enc, 12, dir * 4);
              await ui.settle(50);
              const r = await rate((scan.length === 1 ? 0.3 : 0.15) * RATE); // the first row is the audio window, before the echo
              scan.push({ enc, dir, ...r });
              if (r.rate < 0.02 && marks.found === undefined) marks.found = scan.length - 1;
            }
          }
          await until(clock, (await clock()) + 0.4 * RATE);
        });
        log.stereoConfigScan = scan; // rate: stamps cleared per block (32 frames)
        expect(scan.every((row) => (row.rate as number) < 0.02), 'no guard refuses at any FLTR position').toBe(true);
        const hit = marks.found === undefined ? undefined : (scan[marks.found] as { from: number; to: number; enc: string; dir: number });
        log.stereoConfigFound = hit ?? 'none: a guard refused at every knob position of FLTR page 1';
        if (hit) log.stereoConfigSplit = expectSplitPair('config', c, hit.from, hit.to, 440, 660);
      });

      // Last: the pan phase leaves the track pan turned hard right, so no later phase may depend on the pan.
      // Pan: which AMP page and encoder carries PAN is not known for the DN2 (params 61..72 are the AMP group, PAN is 71): try the candidates in turn and
      // stop at the first that moves the balance. The check is the movement, hard left against hard right.
      await phase('pan', async () => {
        const g = fixtures.s!;
        expect(await api.select(file('s'), 0)).toBe(0);
        expect((await settle(0)).state).toBe(3);
        void g;
        // Balance: the level of L against the level of R over both source tones (a stereo source is unaffected by which tone is in which channel).
        const balance = (c: Awaited<ReturnType<typeof holdNote>>, from: number, to: number) => {
          const l = tone(c.l, 440, from, to) + tone(c.l, 660, from, to);
          const r = tone(c.r, 440, from, to) + tone(c.r, 660, from, to);
          return { l, r, ratio: l / Math.max(r, 1e-9) };
        };
        // Run 1 found it: AMP page 2, encoder G (the other candidates did not move the balance).
        const candidates: [number, string][] = [
          [2, 'ENC G'],
          [1, 'ENC G'],
        ];
        const tried: Record<string, unknown>[] = [];
        for (const [page, enc] of candidates) {
          await ui.tap('FLTR');
          for (let i = 0; i < page; i++) await ui.tap('AMP');
          const marks: Record<string, number> = {};
          const c = await captureNote(runtime, path.join(artifacts, `pan-${page}-${enc.slice(-1)}.wav`), async (clock) => {
            const t0 = await clock();
            await until(clock, t0 + 0.9 * RATE);
            marks.base = await clock();
            await ui.burst(enc, 40, -4);
            await ui.settle(100);
            await until(clock, (await clock()) + 0.1 * RATE);
            marks.left = await clock();
            await until(clock, marks.left + 0.5 * RATE);
            await ui.burst(enc, 40, 4);
            await ui.settle(100);
            await until(clock, (await clock()) + 0.1 * RATE);
            marks.right = await clock();
            await until(clock, marks.right + 0.5 * RATE);
          });
          const base = balance(c, marks.base! - 0.5 * RATE, marks.base!);
          const left = balance(c, marks.left!, marks.left! + 0.5 * RATE);
          const right = balance(c, marks.right!, marks.right! + 0.5 * RATE);
          tried.push({ page, enc, base, left, right });
          if (left.ratio > right.ratio * 2 && left.ratio > base.ratio * 1.2 && right.ratio < base.ratio * 0.85) {
            log.pan = { page, enc, base, left, right };
            log.panTried = tried;
            return;
          }
        }
        log.panTried = tried;
        throw new Error(`no AMP candidate moved the balance: ${JSON.stringify(tried)}`);
      });

      const modules = await dk.modules();
      log.modules = modules.map((m) => ({ name: m.name, faults: m.faults, off: m.guard_off }));
      for (const m of modules) expect(m.faults, `${m.name} faults`).toBe(0);
      await expectNoNewFault('the end');
      expect(failures, `phase failures:\n${failures.join('\n')}`).toEqual([]);
    } finally {
      try {
        // Diagnostic when the body threw outside a phase: the daemon's view of the CF and the faults.
        log.statusAtEnd = await session.client.status();
        log.faultsAtEnd = (await session.faults()).slice(-8);
      } catch (error) {
        log.statusAtEnd = `unavailable: ${error instanceof Error ? error.message : String(error)}`;
      }
      record(artifacts, 'measurements', log);
      console.info('shot-dn2 measurements', JSON.stringify(log));
      try {
        record(artifacts, 'runtime-log', await dk.readLog());
      } catch {
        // The log is a diagnostic only.
      }
    }
  },
  { devices: ['dn2'], patchSet: 'sample-tracks', timeout: 1_800_000 },
);
