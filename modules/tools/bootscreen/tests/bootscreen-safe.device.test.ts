import { expect, test } from 'vitest';
import { button } from '@digi/test-support';
import { currentRecord } from '@digi/regression/runtime/card-kit.ts';
import { CARD_SYMBOLS, cardRigs, RT } from '@digi/regression/runtime/card-rig.ts';
import { CARD_TIMEOUT } from '@digi/regression/runtime/debug-kit.ts';
import { intro, lastFrame, SAFE_COUNTER, SAFE_KEY, spec, valid } from './bootscreen-support.ts';

/** tools/bootscreen, safe mode (see bootscreen.device.test.ts): by the REC key and by the boot counter after resets from the intro callback. */

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
