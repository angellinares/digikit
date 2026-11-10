import { expect, test } from 'vitest';
import { asciiOf } from '@digi/dk-session';
import { bootInfo, bootInfoLoaded } from '@digi/regression/runtime/card-kit.ts';
import { CARD_SYMBOLS, cardRigs, RT } from '@digi/regression/runtime/card-rig.ts';
import { CARD_TIMEOUT } from '@digi/regression/runtime/debug-kit.ts';
import { ringEvents } from '@digi/regression/runtime/runtime-menu.ts';
import { intro, lastFrame, spec, valid } from './bootscreen-support.ts';

/**
 * tools/bootscreen: a custom boot animation as a normal module. The active file is a setting that the card holds before the cold boot (the settings
 * record is laid over the card at the first settings copy, `foreign` of `buildCard`; `tools/bootscreen/active` is the key the runtime stores for the
 * module's own `active`). The intro framebuffer after the boot is the last frame the intro drew.
 * This file: the intro of a valid or an unusable file, and the tick order. Safe mode and the fault halt are in bootscreen-safe and bootscreen-fault.
 */

cardRigs('boot screen', { relaxFs: true, roots: RT, symbols: CARD_SYMBOLS }, (h) => {
  test(
    'a valid file with active set: the intro shows its frames and the boot reaches the UI',
    async () => {
      await h.load('valid', spec(h, { name: 'test.dkbs', file: valid }));
      const info = await bootInfoLoaded(h.session, h.plan);
      expect(info).toMatchObject({ safe: 0, loaded: 1 });
      expect(await intro(h)).toEqual(lastFrame);
      await expect.poll(async () => (await bootInfo(h.session, h.plan)).uiReached, { timeout: 30_000 }).toBe(1);
    },
    CARD_TIMEOUT,
  );

  test(
    'every tick runs once and in order: the frame callback hash of ticks 0..39 and the tick count',
    async () => {
      const ticks = h.mod('test/intro-ticks', 'intro-ticks');
      await h.load('ticks', { modules: [ticks], selection: [ticks.name] });
      await bootInfoLoaded(h.session, h.plan);
      await expect.poll(async () => (await bootInfo(h.session, h.plan)).uiReached, { timeout: 30_000 }).toBe(1);
      // `done` logs "ticks" and "hash" at the first UI pass
      const logged = new Map<string, number[]>();
      for (const e of await ringEvents(h.session, h.plan, 'MOD_LOG')) {
        const text = asciiOf(...e.args.slice(0, 4));
        logged.set(text, [...(logged.get(text) ?? []), e.args[4]! >>> 0]);
      }
      // h = h * 31 + tick + 1, in uint32 (the fixture's frame callback)
      let want = 0;
      for (let t = 0; t < 40; t++) want = (Math.imul(want, 31) + t + 1) >>> 0;
      expect(logged.get('ticks'), 'done ran once, after 40 ticks').toEqual([40]);
      expect(logged.get('hash'), 'order-dependent hash of ticks 0..39').toEqual([want]);
      console.log(`TICKHASH ${h.device} ${want.toString(16)}`);
    },
    CARD_TIMEOUT,
  );

  let stock: Buffer | undefined;
  for (const [label, o] of [
    ['active unset: the stock intro, no take', { file: valid }],
    ['active names a missing file: the stock intro', { name: 'missing.dkbs', file: valid }],
    ['a corrupt file: the stock intro', { name: 'test.dkbs', file: Buffer.concat([valid.subarray(0, 40), Buffer.from([0xff])]) }],
  ] as const) {
    test(
      label,
      async () => {
        await h.load('stock', spec(h, o));
        const info = await bootInfoLoaded(h.session, h.plan);
        expect(info, 'the module loads and changes nothing').toMatchObject({ safe: 0, loaded: 1 });
        const raw = await intro(h);
        expect(raw.equals(lastFrame), 'not the module frame').toBe(false);
        stock ??= raw;
        expect(raw.equals(stock!), 'the same stock intro every time').toBe(true);
        await expect.poll(async () => (await bootInfo(h.session, h.plan)).uiReached, { timeout: 30_000 }).toBe(1);
      },
      CARD_TIMEOUT,
    );
  }
});
