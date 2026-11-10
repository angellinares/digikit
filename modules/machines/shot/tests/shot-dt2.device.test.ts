import { readFileSync } from 'node:fs';
import path from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { Sample, prepareUpload } from '@digi/dk-session/dksample';
import { openSamples } from '@digi/dk-session/samples';
import { button, repoRoot } from '@digi/test-support';
import { expect } from 'vitest';
import type { PatchedSession } from '@digi/regression/support.ts';
import { liveArtifacts, record } from '@digi/regression/runtime/artifacts.ts';
import { installWithDebug } from '@digi/regression/runtime/install.ts';
import { memory, panel } from '@digi/regression/runtime/panel.ts';
import { runtimeTests } from '@digi/regression/runtime/ready.ts';
import { planSymbol, prepareRuntimePlan } from '@digi/regression/runtime/runtime-plan.ts';
import { expectedWindow, setSmp, trackApi as sharedTrackApi } from './shot-support.ts';

// End to end, DT2 1.17: the device-neutral runtime (API 1.5 sample pool), core/sample-pool (the borrowed stock pool), machine contract 1.2
// and digikit/shot playing a stereo sample that a fixture module added to the pool by path (docs/design/sample-api-1.5.md).
//
// The sample files have a different sine on each channel, so the tones in the captured PCM say which source channel reached the output.
// Everything is measured on the coupled PCM of the stock chain (pcm.tap). With the pair gate set by core/dsp-voice the stock pan stage keeps
// the two voices apart: the pair is split (`expectSplitPair`); the other checks take the louder channel of each tone.

const RATE = 48000;
const FOLDER = 'shotproof';
const TRACK = 0; // the pool entry of the played track: SMP is 0 by default
const HANDLE = 1; // the first module with a pool table (digikit/shot)
const SECONDS = 6;

/** A stereo 48 kHz PCM16 WAV: a sine of `left` Hz on L and of `right` Hz on R. 48 kHz keeps the converter exact. */
function stereoWav(seconds: number, left: number, right: number): Uint8Array {
  const frames = seconds * RATE;
  const pcm = Buffer.alloc(frames * 4);
  for (let n = 0; n < frames; n++) {
    pcm.writeInt16LE(Math.round(14000 * Math.sin((2 * Math.PI * left * n) / RATE)), n * 4);
    pcm.writeInt16LE(Math.round(14000 * Math.sin((2 * Math.PI * right * n) / RATE)), n * 4 + 2);
  }
  const h = Buffer.alloc(44);
  h.write('RIFF', 0);
  h.writeUInt32LE(36 + pcm.length, 4);
  h.write('WAVEfmt ', 8);
  h.writeUInt32LE(16, 16);
  h.writeUInt16LE(1, 20);
  h.writeUInt16LE(2, 22);
  h.writeUInt32LE(RATE, 24);
  h.writeUInt32LE(RATE * 4, 28);
  h.writeUInt16LE(4, 32);
  h.writeUInt16LE(16, 34);
  h.write('data', 36);
  h.writeUInt32LE(pcm.length, 40);
  return Uint8Array.from(Buffer.concat([h, pcm]));
}

// ---- measurement on the captured float WAV -------------------------------------------------------------------------------------

interface Capture {
  l: Float32Array;
  r: Float32Array;
  /** The frame of the key press (set by captureNote); a capture can start in the tail of an earlier note. */
  pressedAt?: number;
}

function readCapture(file: string): Capture {
  const wav = readFileSync(file);
  expect(wav.toString('ascii', 0, 4)).toBe('RIFF');
  expect(wav.readUInt16LE(20), 'IEEE float').toBe(3);
  expect(wav.readUInt16LE(22)).toBe(2);
  expect(wav.readUInt32LE(24)).toBe(RATE);
  const frames = Math.floor((wav.length - 44) / 8);
  const l = new Float32Array(frames);
  const r = new Float32Array(frames);
  for (let i = 0; i < frames; i++) {
    l[i] = wav.readFloatLE(44 + i * 8);
    r[i] = wav.readFloatLE(44 + i * 8 + 4);
  }
  return { l, r };
}

const peak = (x: Float32Array, from: number, to: number) => {
  let p = 0;
  for (let i = Math.max(0, from); i < Math.min(x.length, to); i++) p = Math.max(p, Math.abs(x[i]!));
  return p;
};

/** First frame at or after the key press where either channel exceeds the threshold, or -1. */
function onset(c: Capture, threshold = 0.01): number {
  for (let i = c.pressedAt ?? 0; i < c.l.length; i++)
    if (Math.abs(c.l[i]!) > threshold || Math.abs(c.r[i]!) > threshold) return i;
  return -1;
}

/** Amplitude of the sine of `hz` in x over frames [from, to) (Goertzel). A source sine of 14000/32768 reaches about 0.031 after the stock gain (0.4045 applied by SHOT). */
function tone(x: Float32Array, hz: number, from: number, to: number): number {
  const w = (2 * Math.PI * hz) / RATE;
  let re = 0;
  let im = 0;
  const end = Math.min(to, x.length);
  for (let i = Math.max(0, from); i < end; i++) {
    re += x[i]! * Math.cos(w * i);
    im -= x[i]! * Math.sin(w * i);
  }
  return (2 * Math.hypot(re, im)) / Math.max(1, end - from);
}

/** The strongest sine within `spread` (a fraction) of `hz` over frames [from, to), searched in 0.25 Hz steps: the 0.9 s window resolves about
 * 1 Hz, so a pitch that is off by a few cents would miss a fixed-frequency measurement. */
function peakTone(x: Float32Array, hz: number, from: number, to: number, spread = 0.03): { hz: number; amp: number } {
  let best = { hz, amp: 0 };
  for (let f = hz * (1 - spread); f <= hz * (1 + spread); f += 0.25) {
    const amp = tone(x, f, from, to);
    if (amp > best.amp) best = { hz: f, amp };
  }
  return best;
}

const AUDIBLE = 0.006; // 0.015 x 0.4045, the stock gain SHOT now applies
const ABSENT = 0.0016; // 0.004 x 0.4045

/** The louder channel holds each tone of `want` and none of `absent` over frames [from, to). Returns the measured levels. */
function expectTones(name: string, c: Capture, from: number, to: number, want: number[], absent: number[]) {
  const levels = Object.fromEntries(
    [...want, ...absent].map((hz) => [hz, Math.max(tone(c.l, hz, from, to), tone(c.r, hz, from, to))]),
  );
  for (const hz of want)
    expect(levels[hz]!, `${name}: ${hz} Hz in the louder channel (${JSON.stringify(levels)})`).toBeGreaterThan(AUDIBLE);
  for (const hz of absent)
    expect(levels[hz]!, `${name}: ${hz} Hz absent (${JSON.stringify(levels)})`).toBeLessThan(ABSENT);
  return levels;
}

/** The pair is split: the left source (`left` Hz) is in L and the right source (`right` Hz) is in R, each with the other tone at least 20 dB
 * lower in the same channel. The stock pan stage takes the stereo-pair path when the even voice's gate word is 1 (core/dsp-voice sets it;
 * finding 96). The 20 dB limit is 0.1 of the wanted tone. Measured: the other tone is 1e-8 (A) and 1.3e-4 (B, window leakage) against 0.076, that is 55 dB
 * and more down; a summed pair (the old behaviour) gives a ratio of 1. 20 dB leaves a margin of 35 dB for noise without hiding a partial leak. */
function expectSplitPair(name: string, c: Capture, from: number, to: number, left: number, right: number) {
  const measured = {
    lLeft: tone(c.l, left, from, to),
    lRight: tone(c.l, right, from, to),
    rLeft: tone(c.r, left, from, to),
    rRight: tone(c.r, right, from, to),
  };
  const text = JSON.stringify(measured);
  expect(measured.lLeft, `${name}: the left source is in L ${text}`).toBeGreaterThan(AUDIBLE);
  expect(measured.rRight, `${name}: the right source is in R ${text}`).toBeGreaterThan(AUDIBLE);
  expect(measured.lRight, `${name}: the right source is at least 20 dB down in L ${text}`).toBeLessThan(
    measured.lLeft * 0.1,
  );
  expect(measured.rLeft, `${name}: the left source is at least 20 dB down in R ${text}`).toBeLessThan(
    measured.rRight * 0.1,
  );
  return measured;
}

// ---- the device ----------------------------------------------------------------------------------------------------------------

/** The panel path of sample-playback.test.ts: the sole dynamic machine, row 7 of the DT2 machine list, on the selected track (TRIG 1). */
async function selectDynamicMachine({ session }: Pick<PatchedSession, 'session'>, artifacts: string): Promise<string> {
  const ui = panel(session);
  await ui.tap('NO');
  await session.client.inputButton(button('FUNC'), true);
  try {
    await ui.settle(5);
    await ui.tap('SRC');
  } finally {
    await session.client.inputButton(button('FUNC'), false);
  }
  await ui.settle(205);
  for (let i = 0; i < 16; i++) await ui.tap('UP');
  for (let i = 0; i < 7; i++) await ui.tap('DOWN');
  await ui.tap('YES', 300);
  await ui.tap('YES', 300);
  const page = await session.client.framebufferGet({ hash: true, data: true });
  record(artifacts, 'machine-page', page);
  return page.sha256 ?? '';
}

function trackApi({ dk }: PatchedSession) {
  /** `sample_pool_add` through test/track-select: the pool index, or a negative error code. */
  async function add(file: string) {
    const reply = await dk.request(0x3e, Buffer.concat([Buffer.from([HANDLE, 0x61]), Buffer.from(file)]));
    return reply.status !== 0 ? reply.status : Buffer.from(reply.body).readInt32BE(0);
  }
  async function clear(index: number) {
    return (await dk.request(0x3e, Buffer.from([HANDLE, 0x63, index]))).status;
  }
  /** Put `file` in pool entry `index` (the lowest free one): clear it, then add. 0 when it landed there, else an error code or 1000 + the entry. */
  async function select(file: string, index = TRACK) {
    const cleared = await clear(index);
    if (cleared !== 0) return cleared;
    const got = await add(file);
    return got < 0 ? got : got === index ? 0 : 1000 + got;
  }
  /** 0 unused, 1 queued, 2 loading, 3 ready, below 0 an error; the info words when ready. */
  async function status(track = TRACK) {
    const reply = await dk.request(0x3f, Buffer.from([HANDLE, track]));
    expect(reply.status, 'pool status request').toBe(0);
    const b = Buffer.from(reply.body);
    return {
      state: b.readInt32BE(0),
      frames: b.length >= 20 ? b.readUInt32BE(4) : 0,
      rate: b.length >= 20 ? b.readUInt32BE(8) : 0,
      channels: b.length >= 20 ? b.readUInt32BE(12) : 0,
      token: b.length >= 20 ? b.readUInt32BE(16) : 0,
    };
  }
  /** Poll until the track reaches a terminal state (ready or an error); every distinct observation is kept. */
  async function settle(timeoutMs = 90_000, index = TRACK) {
    const seen: { t: number; state: number; token: number }[] = [];
    const t0 = Date.now();
    for (;;) {
      const s = await status(index);
      if (!seen.length || seen.at(-1)!.state !== s.state || seen.at(-1)!.token !== s.token)
        seen.push({ t: Date.now() - t0, state: s.state, token: s.token });
      if (s.state === 3 || s.state < 0) return { ...s, seen, ms: Date.now() - t0 };
      if (Date.now() - t0 > timeoutMs) throw new Error(`pool entry ${index} did not settle: ${JSON.stringify(seen)}`);
      await delay(50);
    }
  }
  return { add, clear, select, status, settle };
}

/** A note on TRIG 1 held while `during` runs; returns the captured PCM. The clock counts stereo frames since the capture began
 * (pcm.tap reports floats, two per frame). */
async function captureNote(
  { session }: Pick<PatchedSession, 'session'>,
  file: string,
  during: (clock: () => Promise<number>) => Promise<void>,
): Promise<Capture> {
  const tapReport = async () =>
    (await session.client.call('pcm.tap', { action: 'report' })) as { samples: number; window_rms: number[] };
  const floats = async () => (await tapReport()).samples;
  await session.client.call('pcm.tap', { action: 'start' });
  await session.client.call('pcm.tap', { action: 'capture', path: file });
  const base = await floats();
  const clock = async () => Math.floor(((await floats()) - base) / 2);
  let pressedAt = 0;
  try {
    // A one-shot keeps sounding after the key is released: wait until the output has been silent for 0.3 s, then press.
    await expect
      .poll(
        async () => {
          const windows = (await tapReport()).window_rms;
          return windows.length >= 3 && windows.slice(-3).every((v) => v < 1e-4);
        },
        { timeout: 40_000, interval: 200 },
      )
      .toBe(true);
    pressedAt = await clock();
    await session.client.inputButton(button('TRIG 1'), true);
    try {
      await during(clock);
    } finally {
      await session.client.inputButton(button('TRIG 1'), false);
    }
  } finally {
    await session.client.call('pcm.tap', { action: 'capture_stop' });
  }
  const capture = readCapture(file);
  const from = pressedAt - RATE / 5;
  const before = Math.max(peak(capture.l, from, pressedAt - 480), peak(capture.r, from, pressedAt - 480));
  expect(before, `silent before the press at frame ${pressedAt}`).toBeLessThan(0.002);
  return { ...capture, pressedAt };
}

async function until(clock: () => Promise<number>, frames: number, timeoutMs = 120_000): Promise<void> {
  await expect.poll(clock, { timeout: timeoutMs, interval: 100 }).toBeGreaterThanOrEqual(frames);
}

// ---- the test ------------------------------------------------------------------------------------------------------------------

runtimeTests(
  'SHOT plays user-selected stereo samples of its pool, picked by SMP, through the runtime and the borrowed stock pool',
  async (runtime) => {
    const { session, dk } = runtime;
    const artifacts = liveArtifacts(session, 'shot-dt2');
    const api = trackApi(runtime);
    const log: Record<string, unknown> = {};
    try {
      await installWithDebug(runtime, 'digikit/shot', 'test/track-select', 'test/stock-pool');
      expect((await dk.modules()).map((m) => m.name)).toEqual(
        expect.arrayContaining(['digikit/shot', 'test/track-select', 'test/stock-pool']),
      );
      const srcPageHash = await selectDynamicMachine(runtime, artifacts);

      // 1. Two stereo files on the card: A has L 440 Hz and R 660 Hz, B has L 330 Hz and R 495 Hz.
      const sample = await Sample.create(
        Uint8Array.from(readFileSync(path.join(repoRoot(), 'apps/web/public/dksample.wasm'))),
      );
      const backend = openSamples('stock-rpc', { session: dk, hashOf: (data) => sample.hash(data) });
      await backend.mkdir(FOLDER);
      const files = [
        prepareUpload(sample, 'a.wav', stereoWav(SECONDS, 440, 660)),
        prepareUpload(sample, 'b.wav', stereoWav(SECONDS, 330, 495)),
      ];
      const uploadStart = Date.now();
      for (const file of files) {
        expect(file.kind).toBe('converted');
        await backend.put(FOLDER, file);
        expect((await backend.verify(FOLDER, file)).ok, `${file.name} verifies on the card`).toBe(true);
      }
      log.uploadMs = Date.now() - uploadStart;

      // 2 and 3. Add A; pool entry 0 goes from unused to ready with the file's native header.
      expect((await api.status()).state, 'entry 0 starts unused').toBe(0);
      expect(await api.select(`/${FOLDER}/a`), 'add takes entry 0').toBe(0);
      const readyA = await api.settle();
      log.selectA = readyA;
      expect(readyA.state, `A reaches ready: ${JSON.stringify(readyA.seen)}`).toBe(3);
      expect(readyA.channels).toBe(2);
      expect(readyA.rate).toBe(RATE);
      expect(Math.abs(readyA.frames - SECONDS * RATE), `frames ${readyA.frames}`).toBeLessThanOrEqual(64);
      expect(readyA.token).not.toBe(0);

      // 3b. Peak windows deep inside A, one of them larger than one 64 KiB read step.
      {
        const shared = sharedTrackApi(runtime);
        const stored = files[0]!.stored;
        const small = Math.floor(readyA.frames * 0.7) + 123;
        const large = Math.floor(readyA.frames * 0.2);
        const cases: [string, number, number][] = [
          ['small', small, 950],
          ['large', large, Math.min(40000, readyA.frames - large)],
        ];
        for (const [name, first, frames] of cases) {
          const got = await shared.awaitWindow(runtime, TRACK, readyA.token, first, frames, 95);
          log[`window.${name}`] = { first, frames, ret: got.ret, ms: got.ms };
          expect(got.ret, `${name} window ready`).toBe(95);
          expect(got.cols, `${name} window columns`).toEqual(expectedWindow(stored, 2, first, frames, 95));
        }
      }

      // 4. A note right after READY (READY means the record is in the DSP): both source channels reach the output.
      const noteA = await captureNote(runtime, path.join(artifacts, 'note-a.wav'), async (clock) => {
        await until(clock, (await clock()) + 1.5 * RATE);
      });
      const onsetA = onset(noteA);
      log.noteA = { onset: onsetA, frames: noteA.l.length };
      expect(onsetA, 'the note is audible').toBeGreaterThanOrEqual(0);
      const winA: [number, number] = [onsetA + 4800, onsetA + RATE];
      log.noteALevels = expectTones('A', noteA, ...winA, [440, 660], []);
      log.noteASplit = expectSplitPair('A', noteA, ...winA, 440, 660);
      await session.expectNoFault('first note');

      // 6. Select B while a note of A plays: the old note stops. B is not retriggered because the key is held.
      let readyAtB = 0;
      let selectAt = 0;
      let stateB: Awaited<ReturnType<typeof api.settle>> | undefined;
      const reselect = await captureNote(runtime, path.join(artifacts, 'reselect.wav'), async (clock) => {
        // The hold counts from the press, not from the start of the capture (see the octave note).
        selectAt = (await clock()) + RATE / 2;
        await until(clock, selectAt);
        expect(await api.select(`/${FOLDER}/b`), 'select B queues').toBe(0);
        stateB = await api.settle();
        readyAtB = await clock();
        await until(clock, readyAtB + 1.5 * RATE);
      });
      log.reselect = { readyAtB, stateB, frames: reselect.l.length };
      expect(stateB?.state, `B reaches ready: ${JSON.stringify(stateB?.seen)}`).toBe(3);
      expect(stateB!.token, 'a new token').not.toBe(readyA.token);
      const onsetR = onset(reselect);
      expect(onsetR, 'A was playing before the select').toBeGreaterThanOrEqual(0);
      expectTones('before the select', reselect, onsetR + 4800, selectAt - 2400, [440, 660], [330, 495]);
      const tailFrom = readyAtB + RATE / 4;
      const tailPeak = Math.max(
        peak(reselect.l, tailFrom, reselect.l.length),
        peak(reselect.r, tailFrom, reselect.r.length),
      );
      log.reselectTail = { tailFrom, tailPeak };
      expect(reselect.l.length - tailFrom, 'the capture covers the tail').toBeGreaterThan(RATE);
      expect(tailPeak, 'the old note is silent after the select').toBeLessThan(0.002);
      await session.expectNoFault('reselect while playing');

      // B plays now (a new note).
      const noteB = await captureNote(runtime, path.join(artifacts, 'note-b.wav'), async (clock) => {
        await until(clock, (await clock()) + 1.5 * RATE);
      });
      const onsetB = onset(noteB);
      expect(onsetB, 'B is audible').toBeGreaterThanOrEqual(0);
      const winB: [number, number] = [onsetB + 4800, onsetB + RATE];
      log.noteBLevels = expectTones('B', noteB, ...winB, [330, 495], [440, 660]);
      log.noteBSplit = expectSplitPair('B', noteB, ...winB, 330, 495);

      // 7. The stock reset (ReloadAllSamples with 1) wipes the pool; the runtime reloads the track from the remembered path.
      const before = await api.status();
      expect(before.state).toBe(3);
      let resetAt = 0;
      const trace: { t: number; state: number; token: number }[] = [];
      // Reset-phase timeline: every poll peeks the track's DSP record (ready, token) and control word (CLOSED) at the table addresses the runtime
      // keeps (`RECORDS`, `CONTROL` of client 0), next to the runtime status and the capture clock.
      const tablePlan = await prepareRuntimePlan(session, artifacts, ['RECORDS', 'CONTROL', '_dv_params']);
      const tableMem = memory(session);
      const tableBase = async (name: string) => (await tableMem.cf(planSymbol(tablePlan, name))) + (name === 'RECORDS' ? 32 : 16) * TRACK;
      const dspWords = async (addr: number, n: number) => {
        const peek = await tableMem.dsp(addr, n * 4);
        const b = Buffer.from(peek.hex, 'hex');
        return peek.unknown ? [] : Array.from({ length: n }, (_, i) => b.readUInt32LE(i * 4));
      };
      const recAddr = await tableBase('RECORDS');
      const ctlAddr = await tableBase('CONTROL');
      const timeline: { ms: number; frame: number; state: number; ready?: number; token?: number; closed?: number }[] = [];
      log.tableAddresses = { recAddr, ctlAddr, now: [await dspWords(recAddr, 2), await dspWords(ctlAddr, 1)] };
      const noteReset = await captureNote(runtime, path.join(artifacts, 'reset.wav'), async (clock) => {
        resetAt = (await clock()) + RATE / 2;
        await until(clock, resetAt);
        const t0 = Date.now();
        const reply = await dk.request(0x3a, Buffer.from([1]));
        expect(reply.status, 'stock-pool reload command').toBe(0);
        // The wipe comes about 1.5 s after the call; the track must leave the old binding, then be ready again.
        for (;;) {
          const s = await api.status();
          const [rec, ctl] = [await dspWords(recAddr, 2), await dspWords(ctlAddr, 1)];
          const last = timeline.at(-1);
          if (!last || last.state !== s.state || last.ready !== rec[0] || last.token !== rec[1] || last.closed !== ctl[0])
            timeline.push({ ms: Date.now() - t0, frame: (await clock()) - resetAt, state: s.state, ready: rec[0], token: rec[1], closed: ctl[0] });
          if (!trace.length || trace.at(-1)!.state !== s.state || trace.at(-1)!.token !== s.token)
            trace.push({ t: Date.now() - t0, state: s.state, token: s.token });
          if (s.state === 3 && s.token !== before.token) break;
          if (Date.now() - t0 > 120_000) throw new Error(`track did not reload: ${JSON.stringify(trace)}`);
          await delay(50);
        }
        await until(clock, (await clock()) + RATE);
      });
      log.reset = { before, trace, frames: noteReset.l.length };
      log.resetTimeline = timeline; // frame: capture clock relative to the reset request (48 frames per ms)
      const after = await api.status();
      expect(after.state, `ready again after the reset: ${JSON.stringify(trace)}`).toBe(3);
      expect(after.token, 'the reload published a new token').not.toBe(before.token);
      expect(after.frames).toBe(before.frames);
      expect(after.channels).toBe(2);
      expect(
        trace.some((s) => s.state !== 3 || s.token !== before.token),
        `the track left the old binding: ${JSON.stringify(trace)}`,
      ).toBe(true);
      // The note that played over the reset ended with the old token. It would run for 6 s, so the capture must end well before that.
      const onsetS = onset(noteReset);
      expect(onsetS, 'B played before the reset').toBeGreaterThanOrEqual(0);
      expectTones('before the reset', noteReset, onsetS + 4800, resetAt - 2400, [330, 495], [440, 660]);
      // The old note stops with the close that the runtime issues when the wipe is reported: the sound ends no later than 50 ms (of capture clock)
      // after the first poll that saw CLOSED. The held key does not retrigger the track after the reload, so the capture stays silent to its end.
      const closedAt = timeline.find((s) => s.closed === 1);
      expect(closedAt, `the control word was set to CLOSED: ${JSON.stringify(timeline)}`).toBeDefined();
      log.resetAudio = { resetFrame: resetAt, silenceFrom: (() => { for (let i = resetAt; i < noteReset.l.length; i++) if (peak(noteReset.l, i, i + 480) < 0.002 && peak(noteReset.r, i, i + 480) < 0.002) return i - resetAt; return -1; })() };
      const silenceFrom = (log.resetAudio as { silenceFrom: number }).silenceFrom;
      expect(silenceFrom, 'the note of the old token stopped').toBeGreaterThanOrEqual(0);
      expect(silenceFrom, `the sound ends with the close: ${JSON.stringify(log.resetAudio)}`).toBeLessThanOrEqual(closedAt!.frame + 2400);
      const endPeak = Math.max(
        peak(noteReset.l, noteReset.l.length - RATE / 2, noteReset.l.length),
        peak(noteReset.r, noteReset.r.length - RATE / 2, noteReset.r.length),
      );
      log.resetTail = { endPeak };
      expect(endPeak, 'the note of the old token stopped').toBeLessThan(0.002);

      const noteAfter = await captureNote(runtime, path.join(artifacts, 'after-reset.wav'), async (clock) => {
        await until(clock, (await clock()) + 1.5 * RATE);
      });
      const onsetAfter = onset(noteAfter);
      expect(onsetAfter, 'plays again after the reload').toBeGreaterThanOrEqual(0);
      log.noteAfterLevels = expectTones(
        'after the reset',
        noteAfter,
        onsetAfter + 4800,
        onsetAfter + RATE,
        [330, 495],
        [440, 660],
      );

      // 4b. SMP picks the pool entry of a note. B is entry 0 (SMP 0, the default), A is added as entry 1. SMP is knob D of the SRC page; it is turned to 1 while
      // B plays (`_dv_params` is rewritten only while a voice renders), then the next note is A. Back to 0 for the pitch step.
      const smpIndex = await api.add(`/${FOLDER}/a`);
      expect(smpIndex, 'A is entry 1 next to B').toBe(1);
      const readyA2 = await api.settle(90_000, smpIndex);
      expect(readyA2.state, `A is ready again as entry 1: ${JSON.stringify(readyA2.seen)}`).toBe(3);
      const readSmp = async () => {
        const peek = await tableMem.dsp(planSymbol(tablePlan, '_dv_params'), 4);
        expect(peek.unknown, 'the SMP word is readable').toBe(0);
        return Buffer.from(peek.hex, 'hex').readUInt32LE(0);
      };
      log.smpTurnToA = await setSmp(runtime, readSmp, smpIndex);
      const noteSmp = await captureNote(runtime, path.join(artifacts, 'smp-a.wav'), async (clock) => {
        await until(clock, (await clock()) + 1.5 * RATE);
      });
      const onsetSmp = onset(noteSmp);
      expect(onsetSmp, 'SMP 1 is audible').toBeGreaterThanOrEqual(0);
      log.smpA = expectSplitPair('SMP 1 plays A', noteSmp, onsetSmp + 4800, onsetSmp + RATE, 440, 660);
      expect(tone(noteSmp.l, 330, onsetSmp + 4800, onsetSmp + RATE), 'B (entry 0) is not heard at SMP 1').toBeLessThan(AUDIBLE);
      log.smpTurnToB = await setSmp(runtime, readSmp, 0);
      await session.expectNoFault('SMP');

      // 5. Pitch: PITCH is 0.375 semitone per unit (0x100 raw), so +32 units (0x6000 from 0x4000) are one octave. PITCH is encoder A on Shot's
      // custom control page (SMP D, PLAY B, START E and END H). The stock encoder accelerates by event rate, so the knob is turned in fast
      // bursts. The read-back is word 4 (PITCH) of `_dv_params`, the parameter words that core/dsp-voice builds every block and passes to SHOT
      // as R12 (the words SHOT actually reads). The buffer is rewritten only while a voice renders, so TRIG 1 is held during the turn.
      const PITCH_OCTAVE = 0x6000;
      const ui = panel(session);
      const mem = memory(session);
      const paramsPlan = tablePlan;
      const pitchAddress = planSymbol(paramsPlan, '_dv_params') + 4 * 4; // a byte-alias address; one little-endian word per parameter
      const readPitch = async () => {
        const peek = await mem.dsp(pitchAddress, 4);
        expect(peek.unknown, 'the parameter word is readable').toBe(0);
        return Buffer.from(peek.hex, 'hex').readUInt32LE(0);
      };
      const pageNow = (await session.client.framebufferGet({ hash: true, data: false })).sha256;
      log.pitchPage = { srcPageHash, pageNow, same: pageNow === srcPageHash };
      await session.client.inputButton(button('TRIG 1'), true);
      let turned: Awaited<ReturnType<typeof ui.turnTo>>;
      try {
        await expect.poll(readPitch, { timeout: 10_000, interval: 100 }).toBe(0x4000);
        turned = await ui.turnTo('ENC A', readPitch, PITCH_OCTAVE);
      } finally {
        await session.client.inputButton(button('TRIG 1'), false);
      }
      log.pitchTurn = { address: pitchAddress, value: turned.value, bursts: turned.bursts, trace: turned.trace };
      // The clock counts from the start of the capture, which can lie seconds before the press (the capture waits for the tail of the turn note to
      // end), so the hold is measured from the press.
      const noteUp = await captureNote(runtime, path.join(artifacts, 'octave.wav'), async (clock) => {
        await until(clock, (await clock()) + 1.5 * RATE);
      });
      const onsetUp = onset(noteUp);
      expect(onsetUp, 'the note is audible').toBeGreaterThanOrEqual(0);
      const [fromUp, toUp] = [onsetUp + 4800, onsetUp + RATE];
      // B sample: L 330 Hz, R 495 Hz. One octave up is L 660 Hz, R 990 Hz. A turn tolerance of 0x40 raw is 0.09 semitone (0.5 percent), so the peak
      // is searched within 3 percent and must lie within 1 percent of the octave.
      const upL = peakTone(noteUp.l, 660, fromUp, toUp);
      const upR = peakTone(noteUp.r, 990, fromUp, toUp);
      const crossLR = peakTone(noteUp.l, 990, fromUp, toUp).amp;
      const crossRL = peakTone(noteUp.r, 660, fromUp, toUp).amp;
      const oldL = tone(noteUp.l, 330, fromUp, toUp);
      const oldR = tone(noteUp.r, 495, fromUp, toUp);
      log.octaveLevels = { upL, upR, crossLR, crossRL, oldL, oldR };
      // The level is compared with the same source at its own pitch (note B): a pitch change must not change it.
      const text = JSON.stringify(log.octaveLevels);
      const refB = (log.noteBLevels as Record<string, number>)['330']!;
      expect(upL.amp / refB, `the octave keeps the level of the unison note (${refB}) ${text}`).toBeGreaterThan(0.85);
      expect(upL.amp / refB, `the octave keeps the level of the unison note (${refB}) ${text}`).toBeLessThan(1.15);
      expect(upL.amp, `L holds the octave of the left source ${text}`).toBeGreaterThan(AUDIBLE);
      expect(upR.amp, `R holds the octave of the right source ${text}`).toBeGreaterThan(AUDIBLE);
      expect(Math.abs(upL.hz / 660 - 1), `L pitch ${upL.hz} Hz`).toBeLessThan(0.01);
      expect(Math.abs(upR.hz / 990 - 1), `R pitch ${upR.hz} Hz`).toBeLessThan(0.01);
      expect(crossLR, `the pair stays split: 990 Hz at least 20 dB down in L ${text}`).toBeLessThan(upL.amp * 0.1);
      expect(crossRL, `the pair stays split: 660 Hz at least 20 dB down in R ${text}`).toBeLessThan(upR.amp * 0.1);
      expect(oldL, `the old 330 Hz is at least 20 dB down ${text}`).toBeLessThan(upL.amp * 0.1);
      expect(oldR, `the old 495 Hz is at least 20 dB down ${text}`).toBeLessThan(upR.amp * 0.1);

      const modules = await dk.modules();
      log.modules = modules.map((m) => ({ name: m.name, faults: m.faults, off: m.guard_off }));
      for (const m of modules) expect(m.faults, `${m.name} faults`).toBe(0);
      await session.expectNoFault('SHOT stereo sample');
    } finally {
      record(artifacts, 'measurements', log);
      console.info('shot-dt2 measurements', JSON.stringify(log));
      try {
        record(artifacts, 'runtime-log', await dk.readLog());
      } catch {
        // The log is a diagnostic only.
      }
    }
  },
  { dsp: 'sharc', devices: ['dt2'], patchSet: 'sample-tracks' },
);
