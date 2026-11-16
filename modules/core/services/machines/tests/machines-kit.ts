import { button, encoder as encoderCode, type Device, type Session } from '@digi/test-support';
import { expect } from 'vitest';
import type { CardRig } from '@digi/regression/runtime/card-rig.ts';
import { bootInfo, E_LOADING } from '@digi/regression/runtime/card-kit.ts';
import { deviceProfile } from '@digi/regression/runtime/device-data.ts';
import { bounded, peak, readFloatWav, rpc, SETTLE, word, type Rt } from '@digi/regression/runtime/fx-live.ts';
import { halfword } from '@digi/regression/runtime/fx-track-ui.ts';
import path from 'node:path';

/** Helpers of the machines service device tests: what the firmware shows after a machine row of the image and `machines.dks` act together. */

/** The module whose machine record gives the card a hard NEED on the machines service, so the harness puts `machines.dks` on the card and the runtime loads it. */
export const SERVICE_CARRIER = 'test/sine';
export const carrierCard = () => ({ modules: [SERVICE_CARRIER], selection: ['modules/sine.dkm'] });

/** Plan symbols that `decodeTables` and `dynType` read (the Digitakt II has no page rows). */
export const tableSymbols = (device: Device) => [
  'DKRT_ASSIGN',
  'machines.rows',
  'machines.count',
  'machines.first',
  'machines.row_size',
  'pf_table',
  'params.first',
  'params.count',
  ...(device === 'dn2' ? ['pages.rows', 'pages.count'] : []),
];

export const ENCODERS = ['ENC A', 'ENC B', 'ENC C', 'ENC D', 'ENC E', 'ENC F', 'ENC G', 'ENC H'] as const;

/** The boot loaded the carrier module and no safe mode. A module that needs the machines service loads only when `services/machines.dks` was found, bound and activated. */
export async function expectServiceLoaded(rt: Rt, rig: CardRig): Promise<void> {
  const { session, ui } = rt;
  // the module's DSP part follows its ColdFire part (result LOADING)
  let info = await bootInfo(session, rig.plan);
  for (let i = 0; i < 50 && info.results.includes(E_LOADING); i++) {
    await ui.wait({ ms: 100, why: SETTLE });
    info = await bootInfo(session, rig.plan);
  }
  expect({ loaded: info.loaded, safe: info.safe, results: info.results.slice(0, info.count) }).toEqual({
    loaded: 1,
    safe: 0,
    results: [0],
  });
}

/** The parameter halfwords of the Sound object of `track` (1 based): the array starts at +0x14, `sound_words` long (`[fx]` of devices/<profile>.toml). */
export async function soundWords(session: Session, track = 1): Promise<number[]> {
  const { fx } = deviceProfile(session.options.device) as unknown as {
    fx: { kit_ptr: number; sound_off: number; sound_stride: number; sound_words: number };
  };
  const base = (await word(session, fx.kit_ptr)) + fx.sound_off + fx.sound_stride * (track - 1) + 0x14;
  return Promise.all(Array.from({ length: fx.sound_words }, (_, i) => halfword(session, base + 2 * i)));
}

/** The indices at which two Sound arrays differ. */
export const changed = (a: number[], b: number[]) => a.flatMap((v, i) => (v === b[i] ? [] : [i]));

/** Turn `encoder` up by `detents` in several events and return the one Sound index it changed and the values before and after (fails when it changed none or several). */
export async function turnChangesOneWord(rt: Rt, encoder: string, detents = 12) {
  const before = await soundWords(rt.session);
  for (let i = 0; i < detents / 4; i++) {
    await bounded(() => rt.session.client.inputTurn(encoderCode(encoder), 4));
    await rt.ui.wait({ ms: 30, why: SETTLE });
  }
  await rt.ui.wait({ ms: 100, why: SETTLE });
  const after = await soundWords(rt.session);
  const diff = changed(before, after);
  expect(diff, `${encoder} changes one parameter word`).toHaveLength(1);
  return { index: diff[0]!, before: before[diff[0]!]!, after: after[diff[0]!]! };
}

/** One note: TRIG 1 held for `ms` with the PCM tap on; the peak of the capture (0 is silence). */
export async function notePeak(rt: Rt, dir: string, ms = 800): Promise<number> {
  const { session } = rt;
  const file = path.join(dir, 'note.wav');
  await rpc(session, 'pcm.tap', { action: 'start' });
  await rpc(session, 'pcm.tap', { action: 'capture', path: file });
  await bounded(() => session.client.inputButton(button('TRIG 1'), true));
  await rt.ui.wait({ ms, why: 'emulated time for the note to sound' });
  await rpc(session, 'pcm.tap', { action: 'capture_stop' });
  await bounded(() => session.client.inputButton(button('TRIG 1'), false));
  await rt.ui.wait({ ms: 100, why: SETTLE });
  return peak(readFloatWav(file));
}
