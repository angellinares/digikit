import { button } from '@digi/test-support';
import { expect, test } from 'vitest';
import { CARD_SYMBOLS, cardRigs } from '@digi/regression/runtime/card-rig.ts';
import { introNow, safeFrame } from '@digi/regression/runtime/intro-frame.ts';
import { bootScreenCard, IB, lastFrame, SAFE_KEY } from './introblank-support.ts';

/** core/introblank, REC held at the boot (see introblank.device.test.ts): the safe-mode screen replaces the black intro. */

cardRigs(
  'introblank with REC held',
  { dsp: 'sharc',
    relaxFs: true,
    roots: IB,
    symbols: CARD_SYMBOLS,
    startEmpty: true,
    initialButtons: [{ code: button('RECORD'), down: true }],
    initial: () => ({ modules: [], selection: [] }),
  },
  (h) => {
    test(
      'REC held: black, then the safe-mode frame, reason KEY, no module loaded',
      async () => {
        await h.boot(await h.card(bootScreenCard(h)));
        const info = await h.ready(true);
        expect(info).toMatchObject({ safe: 1, reason: SAFE_KEY, loaded: 0 });
        const shown = await introNow(h.session);
        expect(shown.equals(safeFrame('key')), 'the SAFE MODE / REC HELD frame').toBe(true);
        expect(shown.equals(lastFrame), 'not the module frame').toBe(false);
      },
    );
  },
);
