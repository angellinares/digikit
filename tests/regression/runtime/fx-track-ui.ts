import { encoder, type Session } from '@digi/test-support';
import type { TestRuntime } from '../support.ts';
import { REDRAW, SETTLE, bounded, captureNote, word, type Rt } from './fx-live.ts';
import type { FxProfile } from './fx-session.ts';
import { wordBase } from './memory.ts';

export async function halfword(session: Session, address: number): Promise<number> {
  const value = await word(session, wordBase(address));
  const offset = address % 4;
  if (offset <= 2) return (value >>> (8 * (2 - offset))) & 0xffff;
  return ((value & 0xff) << 8) | ((await word(session, wordBase(address) + 4)) >>> 24);
}

export async function soundImage(session: Session, profile: FxProfile, track: number): Promise<number[]> {
  const base = (await word(session, profile.fx.kit_ptr)) + profile.fx.sound_off + profile.fx.sound_stride * track;
  const result: number[] = [];
  for (let index = 0; index < Math.floor(profile.fx.sound_stride / 2); index++)
    result.push(await halfword(session, base + 2 * index));
  return result;
}

export async function amountShown(session: Session, profile: FxProfile, track: number, cell = 0): Promise<number> {
  const base = (await word(session, profile.fx.kit_ptr)) + profile.fx.sound_off + profile.fx.sound_stride * track;
  return (await halfword(session, base + 0x14 + 2 * profile.fx.send_indices[cell]!)) >>> 8;
}

/** Legacy fast-event bursts, then single slow detents; preserve acceleration and settling. */
export async function setAmount(rt: Rt, profile: FxProfile, enc: string, track: number, want: number, cell = 0) {
  const { session } = rt;
  for (let attempt = 0; attempt < 200; attempt++) {
    const got = await amountShown(session, profile, track, cell);
    if (got === want) {
      await rt.ui.wait({ ms: 40, why: SETTLE });
      return;
    }
    const error = want - got;
    const fast = Math.abs(error) > 12;
    for (let event = 0; event < (fast ? 2 : 1); event++) {
      await bounded(() => session.client.inputTurn(encoder(enc), (error > 0 ? 1 : -1) * (fast ? 8 : 1)));
      await rt.ui.wait({ ms: fast ? 2 : 40, why: 'spacing of the encoder events' });
    }
    await rt.ui.wait({ ms: 120, why: SETTLE });
  }
  throw new Error(`turning ${enc}: shows ${await amountShown(session, profile, track, cell)}, wanted ${want}`);
}

/** Already-selected input: legacy note boundary offset and 1500-frame hold, without reopening the machine list. */
export async function trackNote(runtime: TestRuntime, file: string, triggerFrame: number): Promise<void> {
  await captureNote(runtime.session, { file, trigger: triggerFrame, tag: 'track send note' });
}

export async function selectTrack(rt: Rt, n: number) {
  await rt.ui.tap(`TRIG ${n}`, { until: { ms: 400, why: REDRAW }, held: 'TRK' });
}

export async function kitMenu(rt: Rt, entry: number) {
  await rt.ui.tap('PRESET', { until: { ms: 500, why: REDRAW } });
  await rt.ui.tap('RIGHT', { until: { ms: 200, why: REDRAW } });
  for (let i = 0; i < 2; i++) await rt.ui.tap('UP', { until: { ms: 100, why: REDRAW } });
  for (let i = 0; i < entry; i++) await rt.ui.tap('DOWN', { until: { ms: 100, why: REDRAW } });
}

export async function kitSlotOne(rt: Rt) {
  for (let i = 0; i < 4; i++) await rt.ui.tap('UP', { until: { ms: 80, why: REDRAW } });
}
