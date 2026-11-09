import { expect } from 'vitest';
import { RATE, expectedWindow } from './shot-support.ts';
import { LAYOUTS, PAGE, dn2Tests, physOf, plane, planePages, type Dn2, type Fixture } from './shot-dn2-support.ts';

// DN2 OS 1.12, the sample pool (core/sample-pool: allocator, LP0 page transport into DDR) through the runtime sample pool (API 1.5/1.6):
// boot state, load, handle, DDR planes, dedup, peak windows, cached reselect, throughput with dsp-link coexistence, a missing path.
// The audio checks are in shot-dn2-audio, the supersede and SMP checks in shot-dn2-supersede (setup: shot-dn2-support.ts).

// [adapter.sample_pool] ddr_base is 0x80531000 in devices/digitone-ii.toml.
const NW32_BASE = 0x1014c400; // sp_nw32(DDR_BASE), docs/design/dn2-sample-pool.md section 1
// Pages per second of emulated DSP time (the ceiling is 1500). A guard against a large fall, not a hardware requirement.
// A page is 2048 mono frames, so a stereo voice streaming at 48 kHz needs about 47 pages/s; 150 is about 3x that.
// Runs measure 187-193 (204 before 2026-11), and blocks vary about 4% between runs, so the rate is not exact.
const THROUGHPUT_FLOOR = 150;
const BLOCKS_PER_SECOND = 1500;
const layoutSeen = 'wordrev';

dn2Tests('DN2 sample pool: load, planes, dedup, windows, reselect, throughput', 'shot-dn2-pool', ['a', 'm', 'x', 't'], async (c: Dn2) => {
  const { api, settle, log, phase, pool, link, clocks, rec, ddr, fixtures, file, dspWords } = c;

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
      const got = await api.awaitWindow(c.runtime, 0, s.token, first, frames, 95);
      log[`window.${name}`] = { first, frames, ret: got.ret, ms: got.ms };
      expect(got.ret, `${name} window ready`).toBe(95);
      expect(got.cols, `${name} window columns`).toEqual(expectedWindow(f.prep.stored, f.channels, first, frames, 95));
    }
  });

  // ---- 5. release and reselect: no transfer ----------------------------------------------------------------------------------
  await phase('release-reselect', async () => {
    // Entry 0 holds A: replace it by X, then by A again; A's block is cached.
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
  await phase('throughput', async () => {
    const f = fixtures.t!;
    const p0 = await pool();
    const l0 = await link();
    const c0 = await clocks();
    const samples: { pages: number; beat: number; elapsed: number; dspFrames: number }[] = [];
    expect(await api.select(file('t'), 0)).toBe(0);
    const ready = await settle(0, 120_000, async () => {
      const [p, l, cl] = [await pool(), await link(), await clocks()];
      samples.push({ pages: p.pages, beat: l.beat, elapsed: cl.elapsed, dspFrames: cl.dspFrames });
    });
    const p1 = await pool();
    const l1 = await link();
    const c1 = await clocks();
    expect(ready.state, `T reaches ready ${JSON.stringify(ready.seen)}`).toBe(3);
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
});
