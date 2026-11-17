import { encoder, type Session } from '@digi/test-support';
import { expect } from 'vitest';
import type { TestRuntime } from '../support.ts';
import { REDRAW, SETTLE, bounded, rpc, word, type Rt } from './fx-live.ts';
import { shot, type FxProfile } from './fx-session.ts';
import { halfword } from './fx-track-ui.ts';
import { wordBase } from './memory.ts';
import { type RuntimePlan } from './runtime-plan.ts';
import { hasServiceSymbols, symbolsOf } from './service-symbols.ts';

export const bits = (value: number) => {
  const bytes = Buffer.alloc(4);
  bytes.writeFloatLE(value);
  return bytes.readUInt32LE();
};
export const knobFloat = (raw: number) =>
  Math.fround(Math.fround(1.27) * Math.fround(Math.fround(raw) / Math.fround(32512)));

/** The original FXPAGE fast-event bursts, including their 100 ms trailing settle. */
async function burst(rt: Rt, enc: string, detents: number, events: number) {
  for (let event = 0; event < events; event++) {
    await bounded(() => rt.session.client.inputTurn(encoder(enc), detents));
    await rt.ui.wait({ ms: 2, why: 'fast encoder events: the legacy burst spacing' });
  }
  await rt.ui.wait({ ms: 100, why: 'trailing settle after an encoder burst' });
  await bounded(() => rt.session.expectNoFault(`FX page burst ${enc}`));
}

export function pageKit(rt: Rt, profile: FxProfile, row = 0) {
  const { session } = rt;
  const fx = profile.fx;
  const address = async (index: number) => (await word(session, fx.kit_ptr)) + fx.array_off + 2 * index;
  const raw = async (knob: number) => halfword(session, await address(fx.value_indices[row * 8 + knob]!));
  return {
    raw,
    async snapshot() {
      const base = await address(0);
      return Promise.all(
        Array.from({ length: Math.floor(fx.tail_off / 2) }, (_, i) => halfword(session, base + 2 * i)),
      );
    },
    async poke(index: number, value: number) {
      const base = await address(index);
      for (let i = 0; i < 2; i++) {
        const at = base + i;
        const shift = 8 * (3 - (at % 4));
        const old = await word(session, wordBase(at));
        const byte = (value >>> (8 * (1 - i))) & 0xff;
        await rpc(session, 'mem.cf.poke', {
          addr: wordBase(at),
          value: ((old & ~(0xff << shift)) | (byte << shift)) >>> 0,
        });
      }
    },
    async setRaw(enc: string, knob: number, target: number) {
      for (let attempt = 0; attempt < 120; attempt++) {
        const error = target - (await raw(knob));
        if (Math.abs(error) <= 0x200) {
          await rt.ui.wait({ ms: 60, why: SETTLE });
          return raw(knob);
        }
        // Near the target one detent is smaller than the +-0x200 window; three fast events overshoot it.
        if (Math.abs(error) < 0x1000) await burst(rt, enc, Math.sign(error), 1);
        else await burst(rt, enc, error > 0 ? 8 : -8, 20);
      }
      throw new Error(`turning ${enc}: value ${await raw(knob)}, wanted ${target}`);
    },
    async minimum(enc: string) {
      await burst(rt, enc, -8, 80);
      await rt.ui.wait({ ms: 60, why: SETTLE });
    },
    async maximum(enc: string, knob: number) {
      for (let attempt = 0; attempt < 6; attempt++) {
        await burst(rt, enc, 8, 80);
        await rt.ui.wait({ ms: 60, why: SETTLE });
        if ((await raw(knob)) === 0x7f00) return;
      }
    },
  };
}

export async function openPages(rt: Rt) {
  await rt.ui.tap('FX', { until: { ms: 400, why: REDRAW }, held: 'FUNC' });
}

export async function effectPage(rt: Rt) {
  await openPages(rt);
  for (let i = 0; i < 3; i++) await rt.ui.tap('DOWN', { until: { ms: 300, why: REDRAW } });
}

export async function mainPage(rt: Rt) {
  await rt.ui.tap('NO', { until: { ms: 300, why: REDRAW } });
}

export async function stockPages(rt: Rt, artifacts: string, tag: string) {
  await openPages(rt);
  const hashes: string[] = [];
  for (const name of ['del', 'rev', 'cho']) {
    if (hashes.length) await rt.ui.tap('DOWN', { until: { ms: 300, why: REDRAW } });
    hashes.push(await shot(rt.session, artifacts, `${tag}-${name}`));
  }
  return hashes;
}

export async function pageState(runtime: Pick<TestRuntime, 'session'>, plan: RuntimePlan) {
  const sym = symbolsOf(runtime, plan);
  const base = sym('DKRT_FXP');
  const words = async (offset: number, count: number) =>
    Promise.all(Array.from({ length: count }, (_, i) => word(runtime.session, base + offset + 4 * i)));
  const owner = Buffer.alloc(4);
  owner.writeUInt32BE(await word(runtime.session, base + 28));
  return {
    count: await word(runtime.session, sym('DKRT_FXP_COUNT')),
    owner: [...owner],
    sent: await word(runtime.session, base + 16),
    errors: await word(runtime.session, base + 20),
    current: await words(36, 8),
    dsp: await words(292, 8),
  };
}

export async function expectNoPages(
  runtime: Pick<TestRuntime, 'session'>,
  plan: RuntimePlan,
  serviceExpected: boolean,
) {
  // a boot without an effect has no fx service file: the page state is the service's, so the service must be absent and there is no page list to read
  if (!serviceExpected) {
    expect(hasServiceSymbols(runtime), 'no fx service file is loaded').toBe(false);
    return;
  }
  const state = await pageState(runtime, plan);
  expect(state.count, 'stock page list is selected').toBe(0);
  expect(state.owner, 'no page is owned').toEqual([0, 0, 0, 0]);
}

export function changed(before: number[], after: number[]) {
  return before.flatMap((value, i) => (value === after[i] ? [] : [i]));
}

/** CF strings need not be word aligned. Read up to the first NUL using mapped words. */
export async function cstring(session: Session, address: number) {
  const bytes: number[] = [];
  for (let i = 0; i < 256; i++) {
    const at = address + i;
    const byte = ((await word(session, wordBase(at))) >>> (8 * (3 - (at % 4)))) & 0xff;
    if (!byte) return Buffer.from(bytes).toString('latin1');
    bytes.push(byte);
  }
  throw new Error(`unterminated FX page string at ${address.toString(16)}`);
}
