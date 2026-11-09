import path from 'node:path';
import { expect } from 'vitest';
import { AUDIBLE, RATE, allFinite, captureNote, expectSplitPair, onset, peak, setSmp, tone, until } from './shot-support.ts';
import { dn2Tests, planePages, type Dn2 } from './shot-dn2-support.ts';

// DN2 OS 1.12, load control while a sample plays: a load that is superseded by another, and the SMP value that picks the pool entry of a note.
// Pool, DDR and audio checks are in shot-dn2-pool and shot-dn2-audio (setup: shot-dn2-support.ts). SMP uses the entries that the supersede phase
// leaves (T in entry 0, Y in entry 1), so the two phases stay in this file in that order.

dn2Tests('DN2 supersede and SMP entry choice', 'shot-dn2-supersede', ['t', 'l', 'y'], async (c: Dn2) => {
  const { runtime, api, settle, mem, log, phase, sym, pool, link, rec, fixtures, file, artifacts, expectNoNewFault } = c;

  // T plays from entry 0 in both phases.
  expect(await api.select(file('t'), 0), 'select T').toBe(0);
  expect((await settle(0)).state, 'T is loaded').toBe(3);

  // ---- 7. supersede: T plays (entry 0), L loads in entry 1 and Y replaces it after ~20 pages -------------------------------
  await phase('supersede', async () => {
    const y = fixtures.y!;
    const marks: Record<string, number> = {};
    const info: Record<string, unknown> = {};
    const cap = await captureNote(runtime, path.join(artifacts, 'supersede.wav'), async (clock) => {
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
      if (Math.max(peak(cap.l, from, from + 4800), peak(cap.r, from, from + 4800)) < 0.01) quiet.push(from);
    log.supersedeQuietWindows = quiet;
    expect(allFinite(cap.l) && allFinite(cap.r), 'finite output').toBe(true);
    expect(quiet, 'the tone never goes silent during the supersede').toEqual([]);
    expect(tone(cap.l, 440, marks.press! + 0.3 * RATE, end), 'L tone present').toBeGreaterThan(AUDIBLE);
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
      const cap = await captureNote(runtime, path.join(artifacts, `${name}.wav`), async (clock) => {
        await until(clock, (await clock()) + 1.2 * RATE);
      });
      const o = onset(cap);
      expect(o, `${name} is audible`).toBeGreaterThanOrEqual(0);
      return expectSplitPair(name, cap, o + 4800, o + RATE, hzL, hzR);
    };
    log.smpTurnToL = await setSmp(runtime, readSmp, index);
    log.smpL = await note('smp-l', 220, 330);
    log.smpTurnToT = await setSmp(runtime, readSmp, 0);
    log.smpT = await note('smp-t', 440, 660);
    await expectNoNewFault('smp');
  });
});
