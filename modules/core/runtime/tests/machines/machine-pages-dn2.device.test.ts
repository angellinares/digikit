import { expect, test } from 'vitest';
import { cardRig, RT } from '@digi/regression/runtime/card-rig.ts';
import { FIRMWARE, describeOn } from '@digi/regression/runtime/devices.ts';
import { pickSine, REDRAW, word } from '@digi/regression/runtime/fx-live.ts';
import { decodeTables, dynType } from '@digi/regression/runtime/project-kit.ts';
import { ENCODERS, expectLoaded, namedKnobs, tableSymbols, turnChangesOneWord } from './machines-kit.ts';

/**
 * The SYN key cycles the pages of a run-time machine with two pages on the Digitone II (`test/dyn-dn2-pages`: machine DynPages, pages PGA with ALPH BETA GAMA and
 * PGB with EPSL ZETA). The page rows are the declared ones. Which page is current shows in what a knob edits: the first knob of PGA edits the Sound word of ALPH,
 * the first knob of PGB the word of EPSL, `slot` apart. SYN once gives PGA, twice PGB, three times PGA again. No pixels. One boot; SHARC for the module's DSP part.
 */
const ID = 'test/dyn-dn2-pages';
const AUDIO_MIRROR_DN2_112 = 0x8000487c; // SRAM, first byte: the selected machine type

describeOn('dn2', `DN2 ${FIRMWARE.dn2} machines.dks: two pages of a run-time machine`, () => {
  const run = cardRig('dn2', 'machine-pages-dn2', {
    dsp: 'sharc',
    roots: RT,
    symbols: tableSymbols('dn2'),
    initial: () => ({ modules: [ID], selection: ['modules/dyn-dn2-pages.dkm'] }),
  });

  test('SYN goes PGA, PGB, PGA; each page edits its own parameters', async () => {
    const rt = await run.first();
    const { session } = rt;
    await expectLoaded(rt, run);

    const tables = await decodeTables(session, run.plan);
    expect(tables.machines['DynPages'], 'the machine row').toMatchObject({ short: 'DPG', clone_of: 0, own_dsp: 1 });
    const pages = tables.pages.filter((p) => p.machine === 'DynPages').sort((x, y) => (x.name < y.name ? -1 : 1));
    expect(pages.map((p) => [p.name, p.title])).toEqual([
      ['PGA', 'Pages A'],
      ['PGB', 'Pages B'],
    ]);
    const layout = pages.map((p) => namedKnobs(p.knobs).map((k) => ({ ...k, slot: p.slots[k.knob]! })));
    expect(layout[0]!.map((k) => k.name)).toEqual(['Pg Alpha', 'Pg Beta', 'Pg Gamma']);
    expect(layout[1]!.map((k) => k.name)).toEqual(['Pg Epsilon', 'Pg Zeta']);
    expect(new Set(layout.flat().map((k) => k.slot)).size, 'five parameters, five slots').toBe(5);
    expect(tables.params['Pg Gamma']).toMatchObject({ min: 0x100, max: 0x7f00, default: 0x7000 });

    const type = await dynType(session, run.plan, ID, 'dyn_pages');
    await pickSine(rt, 5);
    await rt.ui.tap('TRIG 1', { until: { ms: 500, why: REDRAW } });
    expect((await word(session, AUDIO_MIRROR_DN2_112)) >>> 24, 'the audio mirror holds the committed type').toBe(type);

    const [a0, b0] = [layout[0]![0]!, layout[1]![0]!];
    const edit = async (k: { knob: number }) => (await turnChangesOneWord(rt, ENCODERS[k.knob]!)).index;
    const syn = () => rt.ui.tap('SYN', { until: { ms: 400, why: REDRAW } });

    await syn(); // page A
    const alpha = await edit(a0);
    await syn(); // page B
    const epsilon = await edit(b0);
    expect(epsilon - alpha, 'on page B the first knob edits EPSL, as far from ALPH as their slots').toBe(b0.slot - a0.slot);
    await syn(); // page A again
    expect(await edit(a0), 'on the third SYN the first knob edits ALPH again').toBe(alpha);
    await session.expectNoFault('two pages cycled and edited');
  }, 900_000);
});
