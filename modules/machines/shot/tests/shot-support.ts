import { readFileSync } from 'node:fs';
import { setTimeout as delay } from 'node:timers/promises';
import { button } from '@digi/test-support';
import { expect } from 'vitest';
import type { PatchedSession } from '@digi/regression/support.ts';
import { record } from '@digi/regression/runtime/artifacts.ts';
import { memory, panel } from '@digi/regression/runtime/panel.ts';

// Helpers of the SHOT device tests (shot-dn2): WAV fixtures, the sample pool requests of test/track-select, PCM capture with the
// key held and the tone measurements on it. shot-dt2.device.test.ts has its own copies of the same code (it passed before they were split out).

export const RATE = 48000;
export const HANDLE = 1; // the first module with a pool table (digikit/shot)

/** A PCM16 48 kHz WAV: one sine per channel (`hz` has one entry for mono, two for stereo), amplitude 14000 times `shape(frame)`.
 * `hz` can also be a function of the frame (a sample that changes its tones over time). */
export function wav(
  seconds: number,
  hz: number[] | ((frame: number) => number[]),
  shape: (frame: number, frames: number) => number = () => 1,
): Uint8Array {
  const channels = (typeof hz === 'function' ? hz(0) : hz).length;
  const frames = Math.round(seconds * RATE);
  const pcm = Buffer.alloc(frames * channels * 2);
  for (let n = 0; n < frames; n++) {
    const tones = typeof hz === 'function' ? hz(n) : hz;
    for (let c = 0; c < channels; c++)
      pcm.writeInt16LE(
        Math.round(14000 * shape(n, frames) * Math.sin((2 * Math.PI * tones[c]! * n) / RATE)),
        (n * channels + c) * 2,
      );
  }
  const h = Buffer.alloc(44);
  h.write('RIFF', 0);
  h.writeUInt32LE(36 + pcm.length, 4);
  h.write('WAVEfmt ', 8);
  h.writeUInt32LE(16, 16);
  h.writeUInt16LE(1, 20);
  h.writeUInt16LE(channels, 22);
  h.writeUInt32LE(RATE, 24);
  h.writeUInt32LE(RATE * channels * 2, 28);
  h.writeUInt16LE(channels * 2, 32);
  h.writeUInt16LE(16, 34);
  h.write('data', 36);
  h.writeUInt32LE(pcm.length, 40);
  return Uint8Array.from(Buffer.concat([h, pcm]));
}

// ---- measurement on the captured float WAV -------------------------------------------------------------------------------------

export interface Capture {
  l: Float32Array;
  r: Float32Array;
  /** The frame of the key press (set by captureNote); a capture can start in the tail of an earlier note. */
  pressedAt?: number;
}

export function readCapture(file: string): Capture {
  const w = readFileSync(file);
  expect(w.toString('ascii', 0, 4)).toBe('RIFF');
  expect(w.readUInt16LE(20), 'IEEE float').toBe(3);
  expect(w.readUInt16LE(22)).toBe(2);
  expect(w.readUInt32LE(24)).toBe(RATE);
  const frames = Math.floor((w.length - 44) / 8);
  const l = new Float32Array(frames);
  const r = new Float32Array(frames);
  for (let i = 0; i < frames; i++) {
    l[i] = w.readFloatLE(44 + i * 8);
    r[i] = w.readFloatLE(44 + i * 8 + 4);
  }
  return { l, r };
}

export const peak = (x: Float32Array, from: number, to: number) => {
  let p = 0;
  for (let i = Math.max(0, from); i < Math.min(x.length, to); i++) p = Math.max(p, Math.abs(x[i]!));
  return p;
};

export const allFinite = (x: Float32Array) => x.every((v) => Number.isFinite(v));

/** First frame at or after `from` (default the key press) where either channel exceeds the threshold, or -1. */
export function onset(c: Capture, threshold = 0.01, from = c.pressedAt ?? 0): number {
  for (let i = from; i < c.l.length; i++) if (Math.abs(c.l[i]!) > threshold || Math.abs(c.r[i]!) > threshold) return i;
  return -1;
}

/** Amplitude of the sine of `hz` in x over frames [from, to) (Goertzel). A source sine of 14000/32768 reaches about 0.031 after the stock gain (0.4045 applied by SHOT; 0.076 before it). */
export function tone(x: Float32Array, hz: number, from: number, to: number): number {
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

export const AUDIBLE = 0.006; // 0.015 x 0.4045, the stock gain SHOT now applies

/** The pair is split: the left source (`left` Hz) is in L and the right source (`right` Hz) is in R, each with the other tone at least 20 dB
 * lower in the same channel. Returns the measured amplitudes. */
export function expectSplitPair(name: string, c: Capture, from: number, to: number, left: number, right: number) {
  const m = {
    lLeft: tone(c.l, left, from, to),
    lRight: tone(c.l, right, from, to),
    rLeft: tone(c.r, left, from, to),
    rRight: tone(c.r, right, from, to),
  };
  const text = JSON.stringify(m);
  expect(m.lLeft, `${name}: the left source is in L ${text}`).toBeGreaterThan(AUDIBLE);
  expect(m.rRight, `${name}: the right source is in R ${text}`).toBeGreaterThan(AUDIBLE);
  expect(m.lRight, `${name}: the right source is at least 20 dB down in L ${text}`).toBeLessThan(m.lLeft * 0.1);
  expect(m.rLeft, `${name}: the left source is at least 20 dB down in R ${text}`).toBeLessThan(m.rRight * 0.1);
  return m;
}

// ---- the device ----------------------------------------------------------------------------------------------------------------

/** Row of the first dynamic machine in the machine list: after the stock rows (DT2 7, DN2 5, finding 33). */
const DYNAMIC_ROW = { dt2: 7, dn2: 5 } as const;

/** The panel path of sample-playback.test.ts: FUNC+SRC, the sole dynamic machine, on the selected track (TRIG 1). */
export async function selectDynamicMachine({ session }: Pick<PatchedSession, 'session'>, artifacts: string): Promise<string> {
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
  for (let i = 0; i < DYNAMIC_ROW[session.options.device as 'dt2' | 'dn2']; i++) await ui.tap('DOWN');
  await ui.tap('YES', 300);
  await ui.tap('YES', 300);
  const page = await session.client.framebufferGet({ hash: true, data: true });
  record(artifacts, 'machine-page', page);
  return page.sha256 ?? '';
}

export interface TrackStatus {
  state: number;
  frames: number;
  rate: number;
  channels: number;
  token: number;
}

/** The requests of test/track-select: add or clear (0x3e) and status (0x3f). `file` is the root-relative path of a listing row ("KICKS/K1").
 * The pool picks the entry (`sample_pool_add`: the first free index, or the entry that already holds the path). */
export function trackApi({ dk }: Pick<PatchedSession, 'dk'>, handle = HANDLE) {
  /** `sample_pool_add`: the pool index, or a negative error code. */
  async function add(file: string): Promise<number> {
    const reply = await dk.request(0x3e, Buffer.concat([Buffer.from([handle, 0x61]), Buffer.from(file)]));
    if (reply.status !== 0) return reply.status;
    return Buffer.from(reply.body).readInt32BE(0);
  }
  /** `sample_pool_clear`: 0 or a negative error code. The entry is a hole at once. */
  async function clear(index: number): Promise<number> {
    const reply = await dk.request(0x3e, Buffer.from([handle, 0x63, index]));
    return reply.status;
  }
  /** Put `file` in pool entry `index`: clear it, then add. Add takes the first free entry, so `index` must be the lowest free one (the tests fill the
   * pool from 0 upwards). 0 when the file is in `index`, the add's error code, or 1000 + the index it landed on. */
  async function select(file: string, index: number): Promise<number> {
    const cleared = await clear(index);
    if (cleared !== 0) return cleared;
    const got = await add(file);
    return got < 0 ? got : got === index ? 0 : 1000 + got;
  }
  /** 0 unused, 1 queued, 2 loading, 3 ready, below 0 an error; the info words when ready. */
  async function status(track: number): Promise<TrackStatus> {
    const reply = await dk.request(0x3f, Buffer.from([handle, track]));
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
  /** Poll until the track is ready or an error; every distinct observation is kept. `each` runs after every poll (sampling counters). */
  async function settle(track: number, timeoutMs = 120_000, each?: (s: TrackStatus) => Promise<void>, pollMs = 20) {
    const seen: { t: number; state: number; token: number }[] = [];
    const t0 = Date.now();
    for (;;) {
      const s = await status(track);
      if (each) await each(s);
      if (!seen.length || seen.at(-1)!.state !== s.state || seen.at(-1)!.token !== s.token)
        seen.push({ t: Date.now() - t0, state: s.state, token: s.token });
      if (s.state === 3 || s.state < 0) return { ...s, seen, ms: Date.now() - t0 };
      if (Date.now() - t0 > timeoutMs) throw new Error(`track ${track} did not settle: ${JSON.stringify(seen)}`);
      await delay(pollMs);
    }
  }
  return { add, clear, select, status, settle };
}

/** Turn SMP (knob B of the SHOT page, word 0 of `_dv_params`, index << 8) to pool entry `index`, one detent at a time with TRIG 1 held: the
 * parameter buffer is rewritten only while a voice renders, so a note must sound while the value is read back. The loop reads the word after every
 * detent and turns toward the target (a detent may move one unit or several); it stops on `word >> 8 === index`. Returns the words seen. */
export async function setSmp(
  { session }: Pick<PatchedSession, 'session'>,
  readWord: () => Promise<number>,
  index: number,
  maxTurns = 30,
): Promise<number[]> {
  const ui = panel(session);
  const trace: number[] = [];
  await session.client.inputButton(button('TRIG 1'), true);
  try {
    await ui.settle(200);
    for (let i = 0; i <= maxTurns; i++) {
      const word = await readWord();
      trace.push(word);
      const now = word >> 8;
      if (now === index) return trace;
      if (i === maxTurns) break;
      await ui.burst('ENC B', 1, now < index ? 1 : -1);
      await ui.settle(60);
    }
  } finally {
    await session.client.inputButton(button('TRIG 1'), false);
  }
  throw new Error(`SMP did not reach ${index}: words ${trace.map((w) => w.toString(16))}`);
}

/** The `dk_strack` record of pool entry `track` in the DSP data of the first module with a table (client 0): eight words. `recordsSymbol` is the plan address of
 * the runtime's RECORDS static (the DSP address of client 0's table). */
export async function trackRecord(session: PatchedSession['session'], recordsSymbol: number, track: number) {
  const mem = memory(session);
  const base = (await mem.cf(recordsSymbol)) + 32 * track;
  const peek = await mem.dsp(base, 32);
  const b = Buffer.from(peek.hex, 'hex');
  const w = peek.unknown ? [] : Array.from({ length: 8 }, (_, i) => b.readUInt32LE(i * 4));
  return {
    unknown: peek.unknown,
    ready: w[0],
    token: w[1],
    frames: w[2],
    rate: w[3],
    channels: w[4],
    fmt: w[5],
    baseL: w[6],
    baseR: w[7],
  };
}

/** A note on TRIG 1 held while `during` runs; returns the captured PCM. The clock counts stereo frames since the capture began
 * (pcm.tap reports floats, two per frame). */
export async function captureNote(
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

export async function until(clock: () => Promise<number>, frames: number, timeoutMs = 120_000): Promise<void> {
  await expect.poll(clock, { timeout: timeoutMs, interval: 100 }).toBeGreaterThanOrEqual(frames);
}
