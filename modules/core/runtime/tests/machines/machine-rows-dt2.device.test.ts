import { pollUntil } from '@digi/test-support';
import { expect, test } from 'vitest';
import { cardRig, RT } from '@digi/regression/runtime/card-rig.ts';
import { FIRMWARE, describeOn } from '@digi/regression/runtime/devices.ts';
import { REDRAW, rpc } from '@digi/regression/runtime/fx-live.ts';
import { decodeTables, dynType, selectDt2, trackType } from '@digi/regression/runtime/project-kit.ts';
import { ENCODERS, expectLoaded, namedKnobs, tableSymbols, turnChangesOneWord } from './machines-kit.ts';

/**
 * What `machines.dks` does for a run-time machine on the Digitakt II (`test/sine`: machine Sine, one parameter Level). The row is in the tables, the machine is in the
 * list, YES commits its type on the track, and the SRC page edits the Sound word of its parameter. Replaces the patch-time smoke cell of `test/machine-clone`, see
 * docs/plans/ledgers/smoke-machines-cells.md. One boot, SHARC for the module's DSP part.
 */
const ID = 'test/sine';
const DSP_TYPE_CACHE = 0x255970; // DSP DM: the DSP's copy of the TX type word of track 1 (one little-endian short per track), set by the first frame after a trig (finding 02)

describeOn('dt2', `DT2 ${FIRMWARE.dt2} machines.dks: the SRC page of a run-time machine`, () => {
  const run = cardRig('dt2', 'machine-rows-dt2', {
    dsp: 'sharc',
    roots: RT,
    symbols: tableSymbols('dt2'),
    initial: () => ({ modules: [ID], selection: ['modules/sine.dkm'] }),
  });

  test('rows, list entry, committed type and the SRC knob of Sine', async () => {
    const rt = await run.first();
    const { session } = rt;
    await expectLoaded(rt, run);

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
    const cache = await pollUntil(
      rt.clock,
      () => rpc<{ hex: string; unknown: number }>(session, 'dsp.peek', { addr: DSP_TYPE_CACHE, len: 2 }),
      (r) => r.unknown === 0 && Buffer.from(r.hex, 'hex').readUInt16LE(0) === type,
      { withinMs: 3000, message: 'the DSP type cache of track 1 holds the machine type', intervalMs: 50 },
    );
    expect(Buffer.from(cache.hex, 'hex').readUInt16LE(0), 'the DSP type cache of track 1').toBe(type);

    // the SRC page: the Level knob edits one Sound word (Level starts at its maximum, so it is turned down)
    await rt.ui.tap('SRC', { until: { ms: 300, why: REDRAW } });
    const edit = await turnChangesOneWord(rt, ENCODERS[knobs[0]!.knob]!, -12);
    expect(edit.after, 'Level moved down').toBeLessThan(edit.before);
    await session.expectNoFault('Sine selected, SRC knob turned');
  }, 900_000);
});
