import { expect, test } from 'vitest';
import { bootInfoLoaded, CMD as MAILBOX, currentRecord, pokeWord, setSelection } from '@digi/regression/runtime/card-kit.ts';
import { CARD_SYMBOLS, cardRigs, RT } from '@digi/regression/runtime/card-rig.ts';
import { CARD_TIMEOUT } from '@digi/regression/runtime/debug-kit.ts';
import { FULL_TIER } from '@digi/regression/runtime/devices.ts';
import { advance } from '@digi/regression/runtime/fx-live.ts';
import { voiceRows } from '@digi/regression/runtime/project-kit.ts';
import { planSymbol } from '@digi/regression/runtime/runtime-plan.ts';

/**
 * Select, restart, boot (the `restart` part of the Python boot proof `pk_dkrt_boot`). A card with an empty selection boots with no module.
 * The mailbox SELECT writes both record copies at seq 2. The mailbox restart (`dkrt_restart`) and the emulator's soft reset boot the same
 * card again: the module loads and claims its DSP voice rows. Selecting nothing and restarting frees the rows (full tier).
 *
 * DSP 'sharc': the voice rows live in the DSP registry, and a module's DSP part is acknowledged only by the SHARC.
 */

const MODULES = { dt2: ['test/sine'], dn2: ['test/dyn-dn2', 'test/sine'] } as const;
/** DSP voice rows: test/sine; on DN2 also test/dyn-dn2 (it carries the DSP of test/sine). */
const VOICES = { dt2: 1, dn2: 2 } as const;
const FREE_TYPE = 0xffff_ffff;
/** The stable period of the proof's records (DSP blocks): short, so the UI loop clears the boot counter within the settle time. */
const STABLE_BLOCKS = 300;
/** Emulated ms the first UI-loop passes run after a boot before the test looks (Python `SETTLE_MS`). */
const SETTLE_MS = 1500;

cardRigs(
  'boot restart',
  {
    dsp: 'sharc',
    roots: RT,
    symbols: [...CARD_SYMBOLS, 'voices.rows', 'dyndsp_info'],
  },
  (h) => {
    test(
      'select through the mailbox, restart: the next boot loads it; select nothing, restart: it is gone',
      async () => {
        const mods = MODULES[h.device].map((id, i) => h.mod(id, `m${i}`));
        const paths = mods.map((m) => m.name);
        // The card holds the modules in its store but selects none.
        const { session } = await h.boot(
          await h.card({ modules: mods, selection: [], stableBlocks: STABLE_BLOCKS }),
        );
        await h.ready();
        const b0 = await bootInfoLoaded(session, h.plan);
        expect(b0, 'a card with an empty selection boots with no module').toMatchObject({
          count: 0,
          loaded: 0,
          safe: 0,
          seq: 1,
        });

        expect(await setSelection(session, h.plan, paths), 'the mailbox selection answers 0').toBe(0);
        const recs = await h.records();
        expect(recs[0], 'both copies hold the selection').not.toBeNull();
        expect(recs[0], 'the same sequence number: the copies are equal').toEqual(recs[1]);
        expect(recs[0]?.paths).toEqual(paths);
        expect(recs[0]?.seq).toBe(2);

        /** `dkrt_restart` (mailbox command), then the emulator's soft reset; returns when the new boot has run its first UI-loop passes. */
        const restart = async () => {
          await pokeWord(session, planSymbol(h.plan, 'DKRT_MAILBOX'), MAILBOX.RESTART);
          await h.waitReset();
          await advance(session, SETTLE_MS);
          return bootInfoLoaded(session, h.plan);
        };

        const b1 = await restart();
        expect(b1, `the next boot loads it (results ${b1.results.join(',')})`).toMatchObject({
          loaded: mods.length,
          seq: 2,
        });
        expect(b1.results, 'every module result is 0').toEqual(Array(mods.length).fill(0));
        const rows = (await voiceRows(session, h.plan)).filter(([type]) => type !== FREE_TYPE);
        expect(rows.length, `its voice rows are claimed: ${JSON.stringify(rows)}`).toBe(VOICES[h.device]);
        expect(currentRecord(await h.records())?.counter, 'the UI loop of the new boot cleared the counter').toBe(0);

        if (FULL_TIER) {
          expect(await setSelection(session, h.plan, []), 'select nothing').toBe(0);
          const b2 = await restart();
          expect(b2.count, 'the boot after that loads nothing').toBe(0);
          expect(
            (await voiceRows(session, h.plan)).every(([type]) => type === FREE_TYPE),
            'and the voice rows are free',
          ).toBe(true);
        }
      },
      CARD_TIMEOUT,
    );
  },
);
