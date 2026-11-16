import { expect, test } from 'vitest';
import { cardRig, RT } from '@digi/regression/runtime/card-rig.ts';
import { FIRMWARE, describeOn } from '@digi/regression/runtime/devices.ts';
import { pickSine, REDRAW, word } from '@digi/regression/runtime/fx-live.ts';
import { decodeTables, dynType } from '@digi/regression/runtime/project-kit.ts';
import { ENCODERS, expectLoaded, namedKnobs, tableSymbols, turnChangesOneWord } from './machines-kit.ts';

/**
 * What `machines.dks` does for a run-time machine with its own page on the Digitone II (`test/dyn-dn2`: machine DynSyn, one SYN page DYN with four parameters).
 * The loaded module's rows are in the tables (names, titles, ranges, knob layout, slots), the machine is in the list at the first place after MIDI, YES commits its
 * type (audio mirror), the SYN key draws its page and a knob edits the Sound word of its parameter. Replaces the patch-time smoke cells of `test/dn2-clone` and
 * `test/dn2-pages`, see docs/plans/ledgers/smoke-machines-cells.md. One boot, SHARC for the module's DSP part.
 */
const ID = 'test/dyn-dn2';
const AUDIO_MIRROR_DN2_112 = 0x8000487c; // SRAM, first byte: the selected machine type

describeOn('dn2', `DN2 ${FIRMWARE.dn2} machines.dks: the own page of a run-time machine`, () => {
  const run = cardRig('dn2', 'machine-rows-dn2', {
    dsp: 'sharc',
    roots: RT,
    symbols: tableSymbols('dn2'),
    initial: () => ({ modules: [ID], selection: ['modules/dyn-dn2.dkm'] }),
  });

  test('rows, list entry, committed type, SYN page and knobs of DynSyn', async () => {
    const rt = await run.first();
    const { session } = rt;
    await expectLoaded(rt, run);

    const tables = await decodeTables(session, run.plan);
    expect(tables.machines['DynSyn'], 'the machine row').toMatchObject({ short: 'DSY', clone_of: 0, own_dsp: 1 });
    const pages = tables.pages.filter((p) => p.machine === 'DynSyn');
    expect(pages.map((p) => [p.name, p.title])).toEqual([['DYN', 'Dyn Synth']]);
    const page = pages[0]!;
    const layout = namedKnobs(page.knobs).map((k) => ({ ...k, slot: page.slots[k.knob]! }));
    expect(layout.map((k) => k.name)).toEqual(['Dyn Alpha', 'Dyn Beta', 'Dyn Gamma', 'Dyn Delta']);
    expect(new Set(layout.map((k) => k.slot)).size, 'every knob has its own slot').toBe(4);
    expect(tables.params['Dyn Gamma']).toMatchObject({ min: 0x100, max: 0x7f00, default: 0x7000 });
    expect(tables.params['Dyn Beta']).toMatchObject({ min: 0, max: 0x3f00, default: 0x1000 });

    // FUNC+SRC, five DOWN (the place of the first run-time machine, as in the round-trip test), YES, YES; a TRIG sends the type to the audio side
    const type = await dynType(session, run.plan, ID, 'dyn_pluck');
    expect(type, 'a run-time type (the range of core/dynreg)').toBeGreaterThanOrEqual(16);
    await pickSine(rt, 5);
    await rt.ui.tap('TRIG 1', { until: { ms: 500, why: REDRAW } });
    expect((await word(session, AUDIO_MIRROR_DN2_112)) >>> 24, 'the audio mirror holds the committed type').toBe(type);

    // SYN draws the own page: the screen is not the one before
    const before = await session.frameHash();
    await rt.ui.tap('SYN', { until: { ms: 400, why: REDRAW } });
    expect(await session.frameHash(), 'the SYN key shows the page').not.toBe(before);

    // each knob edits the Sound word of its parameter: the word indices are as far apart as the slots
    const first = layout[0]!;
    let firstIndex = -1;
    for (const k of layout) {
      const edit = await turnChangesOneWord(rt, ENCODERS[k.knob]!);
      expect(edit.after, `${k.name} moved up`).toBeGreaterThan(edit.before);
      if (firstIndex < 0) firstIndex = edit.index;
      expect(edit.index - firstIndex, `${k.name} is as far from ${first.name} as their slots`).toBe(k.slot - first.slot);
    }
    await session.expectNoFault('DynSyn selected, page shown, knobs turned');
  }, 900_000);
});
