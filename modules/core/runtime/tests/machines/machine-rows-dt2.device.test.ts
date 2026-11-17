import { pollUntil } from '@digi/test-support';
import { expect, test } from 'vitest';
import { captureRemoteOled } from '@digi/regression/runtime/remote.ts';
import { cardRig, RT } from '@digi/regression/runtime/card-rig.ts';
import { FIRMWARE, describeOn } from '@digi/regression/runtime/devices.ts';
import { portAnchorAddress } from '@digi/regression/runtime/device-data.ts';
import { dspBytes, REDRAW, stableFrameHash, type Rt } from '@digi/regression/runtime/fx-live.ts';
import { decodeTables, dynType, selectDt2, trackType } from '@digi/regression/runtime/project-kit.ts';
import { ENCODERS, expectLoaded, namedKnobs, tableSymbols, turnChangesOneWord } from './machines-kit.ts';

/**
 * What `machines.dks` does for a run-time machine on the Digitakt II (`test/sine`: machine Sine, one parameter Level). The row is in the tables, the machine is in the
 * list, YES commits its type on the track, and the SRC page edits the Sound word of its parameter (exactly the mirror index of the Level row). The stock SRC, FLTR and AMP
 * frames of a track are the same with and without the module on the card. Replaces the patch-time smoke cell of `test/machine-clone`, see
 * docs/plans/ledgers/smoke-machines-cells.md. One emulator start, two boots of it (the plain runtime, then the card with the module), SHARC for the module's DSP part.
 */
const ID = 'test/sine';
/** The DSP's copy of the TX type word of track 1 (one short per track, `0x255970 + 2t`), set by the first frame after a trig (finding 02 "DM 0x255970"; finding 49). Resolved from the port anchor of DT2 1.17. */
const DSP_TYPE_CACHE = portAnchorAddress('dt2', FIRMWARE.dt2, 'dsp.voice.value.type_cache');

/** The frames of the stock SRC pages (two presses: the second page), FLTR and AMP of track 1 (the menu frames of the old cell are not compared). */
async function stockFrames(rt: Rt, expected?: string[]): Promise<string[]> {
  const frames: string[] = [];
  for (const key of ['SRC', 'SRC', 'FLTR', 'AMP']) {
    await rt.ui.tap(key, { until: { ms: 300, why: REDRAW } });
    frames.push(await stableFrameHash(rt));
    // with `expected` (the loaded boot): keep the screen of each page that differs from the stock one
    if (expected && frames.at(-1) !== expected[frames.length - 1]) {
      const name = `stock-frame-${frames.length}-${key}`;
      console.info(`${name} screen: ${await captureRemoteOled(rt.session, name)}`);
    }
  }
  return frames;
}

describeOn('dt2', `DT2 ${FIRMWARE.dt2} machines.dks: the SRC page of a run-time machine`, () => {
  const run = cardRig('dt2', 'machine-rows-dt2', {
    dsp: 'sharc',
    roots: RT,
    symbols: tableSymbols('dt2'),
    initial: () => ({}),
  });

  test('rows, list entry, committed type and the SRC knob of Sine', async () => {
    const plain = await run.first();
    const stock = await stockFrames(plain);
    if (new Set(stock).size === 1) {
      // the keys changed nothing on the plain boot: keep the screen and the status for the diagnosis
      const status = await plain.session.status();
      console.info(`plain boot ignored the page keys: mode ${status.mode}, cf ${JSON.stringify(status.cf)}`);
      console.info(`plain-stock screen: ${await captureRemoteOled(plain.session, 'plain-stock')}`);
    }
    expect(new Set(stock).size, 'the stock pages show different frames').toBeGreaterThan(1);

    const rt = await run.load('with-machines', { modules: [ID], selection: ['modules/sine.dkm'] });
    const { session } = rt;
    await expectLoaded(rt, run);
    await session.expectNoFault('machines.dks loaded');
    expect(await stockFrames(rt, stock), 'the stock SRC, FLTR and AMP pages show the same frames with machines.dks loaded').toEqual(stock);

    const tables = await decodeTables(session, run.plan);
    const row = tables.machines['Sine'] as { short: string; clone_of: number; knobs: (string | number)[] };
    expect(row, 'the machine row').toMatchObject({ short: 'SIN', clone_of: 0 });
    const knobs = namedKnobs(row.knobs);
    expect(knobs.map((k) => k.name)).toEqual(['Level']);

    // the list has the entry (the hint is its place with the stock machines) and the choice reaches the track
    const type = await dynType(session, run.plan, ID, 'sine');
    expect(type, 'a run-time type (the range of core/dynreg)').toBeGreaterThanOrEqual(16);
    await selectDt2(rt, 1, type, 7);
    expect(await trackType(session, 1), 'track 1 holds the type of Sine').toBe(type);

    // a trig: the DSP's type cache of track 1 holds the type (the removed clone cell read the selector 8 of its clone there; `equiv-dt2-sine` pins the audio, pending approval)
    await rt.ui.tap('TRIG 1', { until: { drained: true } });
    // the first byte of the short is the type (a type is below 256), as the removed cell read the selector there
    await pollUntil(rt.clock, async () => (await dspBytes(session, DSP_TYPE_CACHE, 1))[0]!, (b) => b === type, {
      withinMs: 3000,
      message: 'the DSP type cache of track 1 holds the machine type',
      intervalMs: 50,
    });

    // the SRC page: the Level knob edits one Sound word (Level starts at its maximum, so it is turned down)
    await rt.ui.tap('SRC', { until: { ms: 300, why: REDRAW } });
    const edit = await turnChangesOneWord(rt, ENCODERS[knobs[0]!.knob]!, -12);
    expect(edit.index, 'Level edits the Sound word of its mirror index').toBe((tables.params['Level'] as { mirror: number }).mirror);
    expect(edit.after, 'Level moved down').toBeLessThan(edit.before);
    await session.expectNoFault('Sine selected, SRC knob turned');
  }, 900_000);
});
