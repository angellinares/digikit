import { button, encoder as encoderCode, type Device, type Session } from '@digi/test-support';
import { expect } from 'vitest';
import type { CardRig } from '@digi/regression/runtime/card-rig.ts';
import { bootInfo, E_LOADING } from '@digi/regression/runtime/card-kit.ts';
import { deviceProfile } from '@digi/regression/runtime/device-data.ts';
import { bounded, SETTLE, word, type Rt } from '@digi/regression/runtime/fx-live.ts';
import { halfword } from '@digi/regression/runtime/fx-track-ui.ts';

/** Helpers of the machine fixture tests on a run-time machine: what the firmware shows after `machines.dks` has acted on the rows the loader made. */

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

/** The one selected module loaded without a result error and without safe mode. A module that needs the machines service loads only when `services/machines.dks` was found and activated. */
export async function expectLoaded(rt: Rt, rig: CardRig): Promise<void> {
  // the module's DSP part follows its ColdFire part (result LOADING)
  let info = await bootInfo(rt.session, rig.plan);
  for (let i = 0; i < 50 && info.results.includes(E_LOADING); i++) {
    await rt.ui.wait({ ms: 100, why: SETTLE });
    info = await bootInfo(rt.session, rig.plan);
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

/** Turn `encoder` by `detents` (negative: down) in events of 4 and return the one Sound index it changed with the values before and after (fails when it changed none or several). */
export async function turnChangesOneWord(rt: Rt, encoder: string, detents = 12) {
  const before = await soundWords(rt.session);
  const step = detents < 0 ? -4 : 4;
  for (let i = 0; i < Math.abs(detents) / 4; i++) {
    await bounded(() => rt.session.client.inputTurn(encoderCode(encoder), step));
    await rt.ui.wait({ ms: 30, why: SETTLE });
  }
  await rt.ui.wait({ ms: 100, why: SETTLE });
  const after = await soundWords(rt.session);
  const diff = changed(before, after);
  expect(diff, `${encoder} changes one parameter word`).toHaveLength(1);
  return { index: diff[0]!, before: before[diff[0]!]!, after: after[diff[0]!]! };
}

/** The knobs of a decoded row or page that name a parameter: the knob index and the name. */
export const namedKnobs = (knobs: (string | number)[]) =>
  knobs.flatMap((k, knob) => (typeof k === 'string' ? [{ knob, name: k }] : []));

export { button };
