import fs from 'node:fs';
import path from 'node:path';
import { decode, encode, type Frame } from '@digi/dkbs';
import { button } from '@digi/test-support';
import { expect, test } from 'vitest';
import { asciiOf } from '@digi/dk-session';
import { bootInfo, bootInfoLoaded, currentRecord } from '@digi/regression/runtime/card-kit.ts';
import { CARD_SYMBOLS, cardRigs, RT, type CardRig, type CardSpec } from '@digi/regression/runtime/card-rig.ts';
import { CARD_TIMEOUT } from '@digi/regression/runtime/debug-kit.ts';
import { encodeSettings } from '@digi/regression/runtime/project-kit.ts';
import { ringEvents } from '@digi/regression/runtime/runtime-menu.ts';

/**
 * tools/bootscreen: a custom boot animation as a normal module. The active file is a setting that the card holds before the cold boot (the settings
 * record is laid over the card at the first settings copy, `foreign` of `buildCard`; `tools/bootscreen/active` is the key the runtime stores for the
 * module's own `active`). The intro framebuffer after the boot is the last frame the intro drew.
 */

const SAFE_COUNTER = 1;
const SAFE_KEY = 2;
const FILE = 'data/tools/bootscreen/test.dkbs';
const ACTIVE = 'tools/bootscreen/active';

const noise = (seed: number) =>
  Uint8Array.from(Array.from({ length: 1024 }, (_, i) => (i * 73 + (i >> 3) * 17 + seed) % 256));
const frames: Frame[] = [
  { bitmap: noise(1), ticks: 10 },
  { bitmap: noise(2), ticks: 10 },
  { bitmap: noise(3), ticks: 10 },
];
const valid = encode(frames);

/** The intro framebuffer of a native bitmap: each 8-byte column is reversed byte for byte and bit for bit. */
function framebufferOf(native: Uint8Array): Buffer {
  const out = Buffer.alloc(1024);
  for (let x = 0; x < 128; x++)
    for (let j = 0; j < 8; j++) {
      let b = native[8 * x + j]!;
      b = ((b & 0xf0) >> 4) | ((b & 0x0f) << 4);
      b = ((b & 0xcc) >> 2) | ((b & 0x33) << 2);
      b = ((b & 0xaa) >> 1) | ((b & 0x55) << 1);
      out[8 * x + 7 - j] = b;
    }
  return out;
}

/** The decoded frame of the last tick, as the intro leaves it on the screen. */
const lastFrame = framebufferOf(decode(valid).frames.at(-1)!.bitmap);

/** A card with the module, an optional data file, and the settings record with `active` set to `name` (none: no record). */
function spec(h: CardRig, o: { name?: string; file?: Uint8Array }): CardSpec {
  const mod = h.mod('tools/bootscreen', 'bootscreen');
  const modules: { name: string; file: string }[] = [mod];
  if (o.file) {
    const file = path.join(h.artifacts, `data-${modules.length}.dkbs`);
    fs.writeFileSync(file, o.file);
    modules.push({ name: FILE, file });
  }
  const foreign: Record<number, Buffer> = {};
  if (o.name !== undefined) {
    const record = encodeSettings(1, [[ACTIVE, Buffer.from(o.name, 'latin1')]]);
    foreign[h.store.setA] = Buffer.concat([record, Buffer.alloc(512 * Math.ceil(record.length / 512) - record.length)]);
  }
  return { modules, selection: [mod.name], foreign };
}

async function intro(h: CardRig) {
  const got = await h.session.client.framebufferGet({ source: 'intro', data: true, hash: true });
  expect(got.source).toBe('intro');
  const raw = Buffer.from(got.data_b64!, 'base64');
  expect(raw.length).toBe(1024);
  return raw;
}

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

cardRigs(
  'boot screen with REC held',
  { dsp: 'sharc',
    relaxFs: true,
    roots: RT,
    symbols: CARD_SYMBOLS,
    initialButtons: [{ code: button('RECORD'), down: true }],
    initial: () => ({ modules: [], selection: [] }),
  },
  (h) => {
    test(
      'REC held from cold with a valid file: safe mode by key, the module is not loaded, the stock intro runs',
      async () => {
        // not `h.load`: it waits for the runtime UI, which a boot in safe mode by key does not reach
        await h.boot(await h.card(spec(h, { name: 'test.dkbs', file: valid })));
        const info = await h.ready(true);
        expect(info).toMatchObject({ safe: 1, reason: SAFE_KEY, loaded: 0 });
        expect((await intro(h)).equals(lastFrame), 'not the module frame').toBe(false);
      },
      CARD_TIMEOUT,
    );
  },
);

// The fault fixture takes the intro and faults in its first frame, on the intro task. The firmware's own handler gets the exception (the runtime
// installs its handler from the UI loop only) and the machine halts, as on the device until the power is cycled. A halted boot ends the daemon
// of the harness before its card can be read back, so the two halves are two daemons: this one proves that the boot halts in the frame function
// (the module's code runs on the intro task, after the module is live), the next one that a card with the counter of two failed boots is safe mode.
cardRigs(
  'boot screen fault halts',
  { dsp: 'sharc', relaxFs: true, roots: RT, symbols: CARD_SYMBOLS, faults: true, deliverExceptions: true },
  (h) => {
    test(
      'a fault in the intro callback halts in the stock handler',
      async () => {
        const fault = h.mod('test/intro-fault', 'intro-fault');
        const err = await h.boot(await h.card({ modules: [fault], selection: [fault.name] }), { timeoutMs: 600_000 }).then(
          () => undefined,
          (e: unknown) => e,
        );
        expect(String(err)).toContain('guest fault during boot: Halted');
      },
      CARD_TIMEOUT,
    );
  },
);

// Why a halted boot is safe to repeat: the boot counter is raised and written before any module code runs (`dkrt_boot` in boot.rs: the `update` call after the
// safe-mode checks, before `loader::EARLY = true` and the first load). A halted boot is not read back (the harness has no halted state to read). The chain
// below proves that the write survives: boots 1 and 2 reset from the intro callback and boot 3 is safe mode by counter, from one card state carried through
// the resets, so a user's power cycles after a halt see the same state.
cardRigs(
  'boot screen reset',
  { relaxFs: true, roots: RT, symbols: CARD_SYMBOLS },
  (h) => {
    test(
      'a reset from the intro callback reaches safe mode on boot 3',
      async () => {
        const reset = h.mod('test/intro-reset', 'intro-reset');
        await h.boot(await h.card({ modules: [reset], selection: [reset.name] }), { timeoutMs: 600_000 });
        const info = await h.ready(true);
        console.log(`RESETCHAIN ${h.device} ${JSON.stringify(info)}`);
        expect(info, 'the third boot is safe mode by counter').toMatchObject({ safe: 1, reason: SAFE_COUNTER, counter: 2, loaded: 0 });
        // the harness consumes the cpu.reset events during the boot: the two resets show as the record's two writes (seq 1, then one per raised counter)
        expect(info.seq, 'two raised counters before the safe boot').toBe(3);
        const raw = await intro(h);
        expect(raw.length).toBe(1024);
        expect(raw.equals(lastFrame), 'the stock intro, not a module frame').toBe(false);
        expect(currentRecord(await h.records())?.paths, 'selection kept').toEqual([reset.name]);
      },
      CARD_TIMEOUT,
    );
  },
);
