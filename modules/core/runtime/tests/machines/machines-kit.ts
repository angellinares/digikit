import { button, encoder as encoderCode, pollUntil, type Device, type Session } from '@digi/test-support';
import { expect } from 'vitest';
import { captureRemoteOled } from '@digi/regression/runtime/remote.ts';
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

/**
 * Digitone II 1.12 ColdFire SRAM addresses. No `devices/fw` anchor or `devices/ports/dn2-1.12.toml` entry holds them (searched), so they are named here; source: finding 02
 * (docs/findings/02-machines-and-parameters.md: "Where the engine parameter words are" for the TX frame, "Changing the machine twice" for the audio mirror) and finding 06 ("DN2 voice slots").
 */
/** The audio side's copy of track 1's sound: the first byte is the selected machine type. */
export const DN2_AUDIO_MIRROR_112 = 0x8000487c;
/** The TX frame the ColdFire sends to the DSP. */
export const DN2_TX_FRAME_112 = 0x80005e60;
/** TX frame word 0x94: the type halfword of track 1 is its upper half. */
export const DN2_TX_TYPE_TRACK1_112 = DN2_TX_FRAME_112 + 0x94;
/** The TX frame halfword of Sound word `slot` (25..65, the mirror index) of voice slot `voice`: `0xda + 0x92 v + 2 (slot - 25)` (finding 06). */
export const dn2VoiceWordAddress = (voice: number, slot: number) => DN2_TX_FRAME_112 + 0xda + 0x92 * voice + 2 * (slot - 25);

/** The engine's word of `slot` in the TX frame block of voice slot `voice`; it follows the Sound word only while the voice's key is held. */
export const voiceWord = (session: Session, voice: number, slot: number) => halfword(session, dn2VoiceWordAddress(voice, slot));

/** Press or release TRIG 1 without a tap (a note sounds while it is down). */
export async function holdTrig(rt: Rt, down: boolean): Promise<void> {
  await bounded(() => rt.session.client.inputButton(button('TRIG 1'), down));
  // `Ui` has no button down/up call: the wait lets the key event drain so a key-up does not leak into the next phase
  if (!down) await rt.ui.wait({ drained: true });
}

export const ENCODERS = ['ENC A', 'ENC B', 'ENC C', 'ENC D', 'ENC E', 'ENC F', 'ENC G', 'ENC H'] as const;

/** The one selected module loaded without a result error and without safe mode. A module that needs the machines service loads only when `services/machines.dks` was found and activated. */
export async function expectLoaded(rt: Rt, rig: CardRig): Promise<void> {
  // the module's DSP part follows its ColdFire part (result LOADING)
  const info = await pollUntil(rt.clock, () => bootInfo(rt.session, rig.plan), (i) => !i.results.includes(E_LOADING), {
    withinMs: 5000,
    message: 'the module DSP part is loaded (no result is LOADING)',
    intervalMs: 100,
  });
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

/** One Sound word (halfword `index` of the array) of `track` (1 based). */
export async function soundWord(session: Session, index: number, track = 1): Promise<number> {
  return (await soundWords(session, track))[index]!;
}

/** The reason of the one fixed wait of a turn that must change nothing: the firmware has no state that says "ignored". */
export const IGNORED = 'emulated time for the firmware to act on a turn that must change nothing (no observable state)';

/** The most bursts a turn sends to get past the lead-in of the encoder filter (finding 03). */
const TURN_BURSTS = 4;

/**
 * `count` encoder events of `step` detents, `gapMs` apart in emulated time, then a settle for the firmware to store the last one. The stock encoder scales a turn by its
 * event rate (12 detents moved a word by 256 at a gap of 30 ms, 3 events of 4 detents), so a test that needs a larger move sends a faster burst.
 */
async function turnEvents(rt: Rt, encoder: string, step: number, count: number, gapMs = 30): Promise<void> {
  for (let i = 0; i < count; i++) {
    await bounded(() => rt.session.client.inputTurn(encoderCode(encoder), step));
    await rt.ui.wait({ ms: gapMs, why: 'the gap between encoder events (the stock encoder scales by event rate)' });
  }
  await rt.ui.wait({ ms: 100, why: SETTLE });
}

/**
 * Turn `encoder` by `detents` (negative: down) in events of 4, 30 ms apart, and repeat the burst (up to TURN_BURSTS times) until a Sound word moves. Waits until a Sound
 * word differs from the one before the turn (the firmware stored the value). Returns the Sound index it changed with the values before and after (fails when it
 * changed none or several). The size of the change depends on how many bursts the lead-in filter swallowed, so it is not a fixed delta (one burst of 12 detents moved a
 * word from 8192 to 8448): a test checks the direction, not an exact step.
 */
export async function turnChangesOneWord(rt: Rt, encoder: string, detents = 12) {
  const before = await soundWords(rt.session);
  // like a hand: the stock encoder filter of an idle control swallows its first counts (finding 03, the lead-in counter), so repeat the burst until a word moves
  for (let n = 0; n < TURN_BURSTS && changed(before, await soundWords(rt.session)).length === 0; n++)
    await turnEvents(rt, encoder, detents < 0 ? -4 : 4, Math.abs(detents) / 4);
  try {
    await rt.ui.wait({
      state: () => soundWords(rt.session),
      done: (w: number[]) => changed(before, w).length > 0,
      withinMs: 3000,
      what: `${encoder} changed a Sound word`,
    });
  } catch (error) {
    console.info(`knob-timeout screen: ${await captureRemoteOled(rt.session, 'knob-timeout')}`);
    throw error;
  }
  const after = await soundWords(rt.session);
  const diff = changed(before, after);
  expect(diff, `${encoder} changes one parameter word`).toHaveLength(1);
  return { index: diff[0]!, before: before[diff[0]!]!, after: after[diff[0]!]! };
}

/** Turn `encoder` by `detents` in events of 4 and return the indices of the Sound words that changed (none is a valid answer: the knob is empty). */
export async function turnChangesNothing(rt: Rt, encoder: string, detents = 12): Promise<number[]> {
  const before = await soundWords(rt.session);
  // as many bursts as `turnChangesOneWord` may need, so that a real knob would have moved (the lead-in filter, finding 03): no change is then not a vacuous pass
  for (let n = 0; n < TURN_BURSTS; n++) await turnEvents(rt, encoder, detents < 0 ? -4 : 4, Math.abs(detents) / 4);
  await rt.ui.wait({ ms: 200, why: IGNORED });
  return changed(before, await soundWords(rt.session));
}

/**
 * Turn `encoder` (bursts of 20 events of `step` detents 2 ms apart, the sign gives the direction) until Sound word `index` holds `limit`, then two bursts more, and return
 * the word: a range that clamps stays at `limit`. A fast burst moves a word by about 0x100 per event. Fails when `limit` is not reached within `maxBursts` bursts or when a
 * word other than `index` changes on the way.
 */
export async function turnPastLimit(rt: Rt, encoder: string, index: number, step: number, limit: number, maxBursts = 20) {
  let words = await soundWords(rt.session);
  for (let n = 0; n < maxBursts && words[index] !== limit; n++) {
    const prev = words;
    await turnEvents(rt, encoder, step, 20, 2);
    words = await soundWords(rt.session);
    expect(changed(prev, words).filter((i) => i !== index), `${encoder}: only word ${index} moves`).toEqual([]);
  }
  expect(words[index], `${encoder} reaches ${limit.toString(16)} within ${maxBursts} bursts`).toBe(limit);
  for (let n = 0; n < 2; n++) await turnEvents(rt, encoder, step, 20, 2);
  return soundWord(rt.session, index);
}

/** The knobs of a decoded row or page that name a parameter: the knob index and the name. */
export const namedKnobs = (knobs: (string | number)[]) =>
  knobs.flatMap((k, knob) => (typeof k === 'string' ? [{ knob, name: k }] : []));

export { button };
