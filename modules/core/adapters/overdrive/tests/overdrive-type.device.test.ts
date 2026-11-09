import path from 'node:path';
import { encoder, type Session } from '@digi/test-support';
import { describe, expect, test } from 'vitest';
import type { PatchedSession } from '@digi/regression/support.ts';
import {
  advance,
  alignedCompare,
  analysisJson,
  bounded,
  capture,
  inspect,
  readFloatWav,
  withGainFxSession,
} from '@digi/regression/runtime/fx-live.ts';
import { tap, type FxProfile } from '@digi/regression/runtime/fx-session.ts';
import { soundImage } from '@digi/regression/runtime/fx-track-ui.ts';
import { triggerFrame } from '@digi/regression/runtime/fixtures.ts';
import { record } from '@digi/regression/runtime/artifacts.ts';
import { FIRMWARE } from '@digi/regression/runtime/devices.ts';

// DT2 only (core/overdrive has a dt2-1.17 port). The FX page's OD.TY knob (ENC H) is the track's OVR TYPE: Sound[68] of the kit,
// value = curve << 9 | post << 8 (README of core/overdrive). The sine fixture plays on track 1.
const OVR_TYPE = 68;
const SOUND_BASE = 0x14 / 2; // halfwords before Sound[0] in a Sound object
const FOLD = 5;
const SINE_HINT = 7; // list position of the sine machine on DT2

/** Turn ENC H until the shown curve is `want` (the knob step depends on the turn size, so close in on the value that the kit holds). */
async function setType(session: Session, profile: FxProfile, want: number): Promise<number> {
  const target = want * 512 + 128;
  for (let attempt = 0; attempt < 80; attempt++) {
    const value = (await soundImage(session, profile, 0))[SOUND_BASE + OVR_TYPE]!;
    if (value >> 9 === want) return value;
    const error = target - value;
    const detents = Math.max(1, Math.min(16, Math.round(Math.abs(error) / 60)));
    await bounded(() => session.client.inputTurn(encoder('ENC H'), Math.sign(error) * detents));
    await advance(session, 60);
  }
  throw new Error(`OVR TYPE did not reach curve ${want}`);
}

describe(`DT2 ${FIRMWARE.dt2} core/overdrive OVR TYPE`, () => {
  const context = withGainFxSession('dt2', 'overdrive OVR TYPE', { initial: 'dry', patchSet: 'overdrive-tracks' });

  test('a non-stock curve changes the note at a high amount, the stock value does not, and amount 0 stays dry', async () => {
    const { fixture, plan, slots, artifacts, dry, rememberEpoch } = context;
    const profile = await analysisJson<FxProfile>(
      'tools.patchkit.pk_digikit_fxroutes',
      ['profile', '--device', 'dt2'],
      artifacts,
      'profile',
    );
    const trigger = triggerFrame('dt2');
    const files: Record<string, string> = {};
    let previous: PatchedSession | undefined;
    for (const [tag, curve, amount] of [
      ['stock-high', 0, true],
      ['fold-high', FOLD, true],
      ['stock-zero', 0, false],
      ['fold-zero', FOLD, false],
    ] as const) {
      const runtime = previous ? await fixture.reload(dry) : fixture.runtime;
      previous = runtime;
      const state = await inspect(runtime, plan, slots, false, artifacts, tag);
      rememberEpoch(state.epoch);
      const { session } = runtime;
      await tap(session, 'FX', 300);
      // OVERDRIVE (ENC B) is a knob of 127 detents: 128 turns it fully up.
      if (amount) {
        for (let i = 0; i < 4; i++) {
          await bounded(() => session.client.inputTurn(encoder('ENC B'), 32));
          await advance(session, 60);
        }
      }
      const shown = curve === 0 ? 0 : await setType(session, profile, curve);
      record(artifacts, `${tag}-type`, { curve, shown });
      files[tag] = path.join(artifacts, `${tag}.wav`);
      await capture(runtime, files[tag]!, SINE_HINT, trigger);
    }
    const pcm = Object.fromEntries(Object.entries(files).map(([tag, file]) => [tag, readFloatWav(file)]));
    const peak = (x: Float32Array) => x.reduce((p, v) => Math.max(p, Math.abs(v)), 0);
    expect(peak(pcm['stock-high']!), 'the stock curve sounds').toBeGreaterThan(1e-4);
    const compare = (a: string, b: string) => alignedCompare(pcm[a]!, pcm[b]!);
    expect(compare('stock-high', 'stock-zero').differ, 'the amount shapes the stock curve').toBeGreaterThan(100);
    expect(compare('stock-high', 'fold-high').differ, 'FOLD differs from the stock curve at a high amount').toBeGreaterThan(100);
    expect(compare('stock-zero', 'fold-zero').differ, 'amount 0 leaves the note as stock for every curve').toBe(0);
  }, 600_000);
});
