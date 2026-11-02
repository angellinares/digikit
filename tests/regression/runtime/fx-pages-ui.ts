import { encoder, type Session } from '@digi/test-support';
import { expect } from 'vitest';
import type { PatchedSession } from '../support.ts';
import { advance, bounded, rpc, word } from './fx-live.ts';
import { shot, tap, type FxProfile } from './fx-session.ts';
import { halfword } from './fx-track-ui.ts';
import { planSymbol, type RuntimePlan } from './runtime-plan.ts';

export const bits = (value: number) => {
  const bytes = Buffer.alloc(4);
  bytes.writeFloatLE(value);
  return bytes.readUInt32LE();
};
export const knobFloat = (raw: number) =>
  Math.fround(Math.fround(1.27) * Math.fround(Math.fround(raw) / Math.fround(32512)));

/** The original FXPAGE fast-event bursts, including their 100 ms trailing settle. */
async function burst(session: Session, enc: string, detents: number, events: number) {
  for (let event = 0; event < events; event++) {
    await bounded(() => session.client.inputTurn(encoder(enc), detents));
    await advance(session, 2);
  }
  await advance(session, 100);
  await bounded(() => session.expectNoFault(`FX page burst ${enc}`));
}

export function pageKit(session: Session, profile: FxProfile, row = 0) {
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
        const old = await word(session, at & ~3);
        const byte = (value >>> (8 * (1 - i))) & 0xff;
        await rpc(session, 'mem.cf.poke', { addr: at & ~3, value: ((old & ~(0xff << shift)) | (byte << shift)) >>> 0 });
      }
    },
    async setRaw(enc: string, knob: number, target: number) {
      for (let attempt = 0; attempt < 120; attempt++) {
        const error = target - (await raw(knob));
        if (Math.abs(error) <= 0x200) {
          await advance(session, 60);
          return raw(knob);
        }
        await burst(session, enc, error > 0 ? 8 : -8, Math.abs(error) < 0x1000 ? 3 : 20);
      }
      throw new Error(`turning ${enc}: value ${await raw(knob)}, wanted ${target}`);
    },
    async minimum(enc: string) {
      await burst(session, enc, -8, 80);
      await advance(session, 60);
    },
    async maximum(enc: string, knob: number) {
      for (let attempt = 0; attempt < 6; attempt++) {
        await burst(session, enc, 8, 80);
        await advance(session, 60);
        if ((await raw(knob)) === 0x7f00) return;
      }
    },
  };
}

export async function openPages(session: Session) {
  await tap(session, 'FX', 400, 'FUNC');
}

export async function effectPage(session: Session) {
  await openPages(session);
  for (let i = 0; i < 3; i++) await tap(session, 'DOWN', 300);
}

export async function mainPage(session: Session) {
  await tap(session, 'NO', 300);
}

export async function stockPages(session: Session, artifacts: string, tag: string) {
  await openPages(session);
  const hashes: string[] = [];
  for (const name of ['del', 'rev', 'cho']) {
    if (hashes.length) await tap(session, 'DOWN', 300);
    hashes.push(await shot(session, artifacts, `${tag}-${name}`));
  }
  return hashes;
}

export async function pageState(runtime: PatchedSession, plan: RuntimePlan) {
  const base = planSymbol(plan, 'DKRT_FXP');
  const words = async (offset: number, count: number) =>
    Promise.all(Array.from({ length: count }, (_, i) => word(runtime.session, base + offset + 4 * i)));
  const owner = Buffer.alloc(4);
  owner.writeUInt32BE(await word(runtime.session, base + 28));
  return {
    count: await word(runtime.session, planSymbol(plan, 'fxp_count')),
    owner: [...owner],
    sent: await word(runtime.session, base + 16),
    errors: await word(runtime.session, base + 20),
    current: await words(44, 8),
    dsp: await words(172, 8),
  };
}

export async function expectNoPages(runtime: PatchedSession, plan: RuntimePlan) {
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
    const byte = ((await word(session, at & ~3)) >>> (8 * (3 - (at % 4)))) & 0xff;
    if (!byte) return Buffer.from(bytes).toString('latin1');
    bytes.push(byte);
  }
  throw new Error(`unterminated FX page string at ${address.toString(16)}`);
}
