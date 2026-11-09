import path from 'node:path';
import { button } from '@digi/test-support';
import { expect } from 'vitest';
import { RATE, allFinite, captureNote, expectSplitPair, onset, tone, until, type Capture } from './shot-support.ts';
import { dn2Tests, type Dn2 } from './shot-dn2-support.ts';

// DN2 OS 1.12, the dsp-voice stereo hooks (docs/design/dn2-stereo.md) with SHOT playing pool samples: L/R tones through the stock filter and pan,
// retrigger, every FLTR page 1 knob position, and the pan encoder. Pool, DDR and supersede checks are in shot-dn2-pool and shot-dn2-supersede
// (setup: shot-dn2-support.ts). Each phase selects the file it plays into entry 0 (`api.select(file, 0)` is clear + add), so no phase depends on another.
// Last: the pan phase leaves the track pan turned hard right, so no later phase may depend on the pan.

dn2Tests('DN2 stereo audio: first note, retrigger, filter types, pan', 'shot-dn2-audio', ['a', 'r', 't', 's'], async (c: Dn2) => {
  const { runtime, session, api, settle, mem, ui, log, phase, sym, dspWords, file, artifacts, expectNoNewFault } = c;

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
  /** Put `name` into entry 0 and wait for it. */
  const load = async (name: string) => {
    expect(await api.select(file(name), 0), `select ${name}`).toBe(0);
    expect((await settle(0)).state, `${name} is ready`).toBe(3);
  };

  // ---- 3. stereo audio: SHOT playing L 440 Hz / R 660 Hz through the stock filter and pan -----------------------------------
  await phase('stereo-audio', async () => {
    await load('a');
    const diag: unknown[] = [];
    const cap = await captureNote(runtime, path.join(artifacts, 'note-a.wav'), async (clock) => {
      await until(clock, (await clock()) + 0.8 * RATE);
      diag.push(await stereoDiag());
      await until(clock, (await clock()) + 0.2 * RATE);
      diag.push(await stereoDiag());
      await until(clock, (await clock()) + 0.5 * RATE);
    });
    log.stereoDiag = diag;
    const o = onset(cap);
    log.noteA = { onset: o, frames: cap.l.length, finite: allFinite(cap.l) && allFinite(cap.r) };
    expect(o, 'the note is audible').toBeGreaterThanOrEqual(0);
    expect(allFinite(cap.l) && allFinite(cap.r), 'finite output').toBe(true);
    log.noteASplit = expectSplitPair('A', cap, o + 4800, o + RATE, 440, 660);
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
    const cap = await captureNote(runtime, path.join(artifacts, 'retrigger.wav'), async (clock) => {
      marks.press = await clock();
      await until(clock, marks.press + 2.3 * RATE);
      await session.client.inputButton(button('TRIG 1'), false);
      await session.client.inputButton(button('TRIG 1'), true);
      marks.retrig = await clock();
      await until(clock, marks.retrig + 1.3 * RATE);
    });
    // The sample plays tones A (440/660) for 1.2 s, then B (880/1320); the sound starts at the key (run 3: A from the press). At the retrigger it is in B (A
    // is only a leak of the stock stages, about half of B); after it A is back at full level within 0.1 s and B has dropped.
    const level = (hz: number[], from: number, to: number) => hz.reduce((m, f) => m + tone(cap.l, f, from, to) + tone(cap.r, f, from, to), 0);
    const [b0, b1] = [marks.retrig! - 0.45 * RATE, marks.retrig! - 0.1 * RATE];
    const [a0, a1] = [marks.retrig! + 0.6 * RATE, marks.retrig! + 1.2 * RATE];
    const m = {
      marks,
      beforeA: level([440, 660], b0, b1),
      beforeB: level([880, 1320], b0, b1),
      afterA: level([440, 660], a0, a1),
      afterB: level([880, 1320], a0, a1),
      finite: allFinite(cap.l) && allFinite(cap.r),
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

  // ---- 9. every filter type plays stereo: scan the FLTR page 1 knobs while T plays and read the rate of `_gs_nbad` (stamps a guard of a twin clears). All
  // stages have an R twin, so the rate stays 0 at every position (run 2, before the twins of the shaper and the held sample, showed R = L with `_gs_nbad` rising
  // by one per block on the default track).
  await phase('stereo-config', async () => {
    await load('t');
    const nbad = async () => (await dspWords('_gs_nbad', 1))[0]!;
    const scan: Record<string, unknown>[] = [];
    const marks: Record<string, number> = {};
    await ui.tap('FLTR');
    const cap = await captureNote(runtime, path.join(artifacts, 'stereo-config.wav'), async (clock) => {
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
    if (hit) log.stereoConfigSplit = expectSplitPair('config', cap, hit.from, hit.to, 440, 660);
  });

  // Pan: which AMP page and encoder carries PAN is not known for the DN2 (params 61..72 are the AMP group, PAN is 71): try the candidates in turn and
  // stop at the first that moves the balance. The check is the movement, hard left against hard right.
  await phase('pan', async () => {
    expect(await api.select(file('s'), 0)).toBe(0);
    expect((await settle(0)).state).toBe(3);
    // Balance: the level of L against the level of R over both source tones (a stereo source is unaffected by which tone is in which channel).
    const balance = (cap: Capture, from: number, to: number) => {
      const l = tone(cap.l, 440, from, to) + tone(cap.l, 660, from, to);
      const r = tone(cap.r, 440, from, to) + tone(cap.r, 660, from, to);
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
      const cap = await captureNote(runtime, path.join(artifacts, `pan-${page}-${enc.slice(-1)}.wav`), async (clock) => {
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
      const base = balance(cap, marks.base! - 0.5 * RATE, marks.base!);
      const left = balance(cap, marks.left!, marks.left! + 0.5 * RATE);
      const right = balance(cap, marks.right!, marks.right! + 0.5 * RATE);
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
});
