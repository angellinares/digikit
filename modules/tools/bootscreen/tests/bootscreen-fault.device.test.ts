import { expect, test } from 'vitest';
import { CARD_SYMBOLS, cardRigs, RT } from '@digi/regression/runtime/card-rig.ts';

/** tools/bootscreen, fault in the intro callback (see bootscreen.device.test.ts). */

// The fault fixture takes the intro and faults in its first frame, on the intro task. The firmware's own handler gets the exception (the runtime
// installs its handler from the UI loop only) and the machine halts, as on the device until the power is cycled. A halted boot ends the daemon
// of the harness before its card can be read back, so the two halves are two daemons: this one proves that the boot halts in the frame function
// (the module's code runs on the intro task, after the module is live), the reset chain in bootscreen-safe that a card with the counter of two failed boots is safe mode.
cardRigs(
  'boot screen fault halts',
  { dsp: 'sharc', relaxFs: true, roots: RT, symbols: CARD_SYMBOLS, faults: true, deliverExceptions: true },
  (h) => {
    test(
      'a fault in the intro callback halts in the stock handler',
      async () => {
        const fault = h.mod('test/intro-fault', 'intro-fault');
        const err = await h.boot(await h.card({ modules: [fault], selection: [fault.name] })).then(
          () => undefined,
          (e: unknown) => e,
        );
        expect(String(err)).toContain('guest fault during boot: Halted');
      },
    );
  },
);
