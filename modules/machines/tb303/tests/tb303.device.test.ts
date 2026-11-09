import { readFileSync } from 'node:fs';
import path from 'node:path';
import { button } from '@digi/test-support';
import { expect } from 'vitest';
import type { PatchedSession } from '@digi/regression/support.ts';
import { liveArtifacts } from '@digi/regression/runtime/artifacts.ts';
import { installWithDebug } from '@digi/regression/runtime/install.ts';
import { panel } from '@digi/regression/runtime/panel.ts';
import { runtimeTests } from '@digi/regression/runtime/ready.ts';
import { settleUi, waitEmulated } from '@digi/regression/runtime/waits.ts';

// DT2 1.17: tom/tb303 (machine contract 1.6, DK_MACHINE_GATE) loads, plays one note on track 1 and ends it after the note-off.
// The audio is the coupled PCM of the stock chain (pcm.tap, 48 kHz stereo float).

const RATE = 48000;
const HOLD_MS = 600;
const TAIL_MS = 3000;
/** The amp's release is at most 2 s (AREL); the tail window starts well after it. */
const SILENT_AFTER_MS = 2500;

/** The dynamic machine is the only one on the card: row 7 of the DT2 machine list, on the selected track (TRIG 1). */
async function selectDynamicMachine({ session }: Pick<PatchedSession, 'session'>): Promise<void> {
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
}

function readCapture(file: string): { l: Float32Array; r: Float32Array } {
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

runtimeTests(
  'TB303 loads on the DT2, plays one note and ends it after the note-off',
  async (runtime) => {
    const { session, dk } = runtime;
    const artifacts = liveArtifacts(session, 'tb303-dt2');
    await settleUi(session);
    await installWithDebug(runtime, 'tom/tb303');
    await settleUi(session);
    expect((await dk.modules()).map((m) => m.name)).toContain('tom/tb303');
    await selectDynamicMachine(runtime);

    const file = path.join(artifacts, 'tb303-note.wav');
    await session.client.call('pcm.tap', { action: 'start' });
    await session.client.call('pcm.tap', { action: 'capture', path: file });
    await waitEmulated(session, 300);
    await session.client.inputButton(button('TRIG 1'), true);
    try {
      await waitEmulated(session, HOLD_MS);
    } finally {
      await session.client.inputButton(button('TRIG 1'), false);
    }
    await waitEmulated(session, TAIL_MS);
    await session.client.call('pcm.tap', { action: 'capture_stop' });
    await session.expectNoFault('tb303 note');

    const { l, r } = readCapture(file);
    for (const x of [l, r]) for (const v of x) expect(Number.isFinite(v), 'finite samples').toBe(true);
    const pressed = Math.floor((RATE * 300) / 1000);
    const heard = Math.max(
      peak(l, pressed, pressed + (RATE * HOLD_MS) / 1000),
      peak(r, pressed, pressed + (RATE * HOLD_MS) / 1000),
    );
    expect(heard, 'the note is audible while held').toBeGreaterThan(0.005);
    // the gate is released: the amp envelope ends the note within SILENT_AFTER_MS of the note-off
    const released = pressed + (RATE * HOLD_MS) / 1000;
    const from = released + (RATE * (SILENT_AFTER_MS - 1000)) / 1000;
    const tail = Math.max(peak(l, from, l.length), peak(r, from, r.length));
    expect(tail, `silent ${SILENT_AFTER_MS - 1000} ms after the note-off (held peak ${heard})`).toBeLessThan(0.002);
  },
  { dsp: 'sharc', devices: ['dt2'], timeout: 600_000 },
);
