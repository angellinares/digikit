import path from 'node:path';
import { expect, test } from 'vitest';
import { cardRigs, RT } from '@digi/regression/runtime/card-rig.ts';
import { fit, needsOf } from '@digi/regression/runtime/dkm.ts';
import { deviceProfile } from '@digi/regression/runtime/device-data.ts';
import { advance, rpc } from '@digi/regression/runtime/fx-live.ts';
import { tap } from '@digi/regression/runtime/fx-session.ts';
import { openDigikitMenu } from '@digi/regression/runtime/runtime-menu.ts';
import { planSymbol, type RuntimePlan } from '@digi/regression/runtime/runtime-plan.ts';
import { packSynth, type SynthSpec } from '@digi/regression/runtime/synth.ts';

/**
 * The MODULES page of SETTINGS > DIGIKIT tells before a selection whether a module will load (docs/plans/what-fits.md; ledger
 * docs/plans/ledgers/debug-card-safe.md, section fits). The fit rule (modules/core/runtime/fit) and the page logic (modules/core/adapters/ui/host/tests/menu.rs)
 * are tested on the host; this test is the real runtime scan path on the real page: a store of four synthetic modules (one small, two a little over half the
 * DSP area each, one larger than the area), the rows read from DK_LOAD. Each row shows the memory the module takes and its kind; YES on the larger one
 * opens the confirm "HUGE DOES NOT FIT" with the reason and ticks nothing; with the first half-size one ticked, the second is refused the same way; unticking
 * the first makes room.
 */

const SYMBOLS = ['DK_LOAD', 'DK_LOAD_N', 'DK_POPUP', 'DK_POPUP_TEXT', 'DK_POPUPS'];

/** TICK is also the name of a runtime static: take the menu's. */
function menuSymbol(plan: RuntimePlan, name: string): number {
  const key = Object.keys(plan.symbols).find((k) => k.startsWith('core/digikit-menu@') && k.endsWith(`:${name}`));
  if (key === undefined) throw new Error(`no menu symbol ${name}`);
  return plan.symbols[key]!;
}

/** The rows of the page for a store (`module_row` of src/menu.c): name without `.dkm` in upper case (12 characters), kind from the needs, memory in K, right aligned to 23 characters. */
function pageRow(file: string, n: Record<string, number>): string {
  const total = n.ARENA! + n.DSP_AREA!;
  const size = total < 1024 ? '<1K' : `${Math.ceil(total / 1024)}K`;
  const kind = n.FX_SLOTS ? 'FX' : n.MACHINE_ROWS ? 'MACH' : 'TOOL';
  const row = path.basename(file, '.dkm').toUpperCase().slice(0, 12).padEnd(13) + kind;
  return row.padEnd(23 - size.length) + size;
}

cardRigs('MODULES page: what fits', { dsp: 'sharc', roots: [...RT, 'core/digikit-menu'], symbols: SYMBOLS }, (rig) => {
  test(
    'a module larger than the DSP area and a second half-size module are refused with the reason; unticking makes room',
    async () => {
      await rig.first();
      const area = deviceProfile(rig.device).digikit.dsp_area_bytes;
      const half = area / 2 + 4096;
      const specs: Record<string, SynthSpec> = {
        'half-a': { name: 'fit/half-a', dsp: { image: 64, bss: half - 64 } },
        'half-b': { name: 'fit/half-b', dsp: { image: 64, bss: half - 64 } },
        huge: { name: 'fit/huge', dsp: { image: 64, bss: area } },
        small: { name: 'fit/small', pad: 64 },
      };
      const dir = path.join(rig.artifacts, 'fits');
      const names = Object.keys(specs).sort(); // the menu lists in path order
      const files = await Promise.all(names.map((k) => packSynth(rig.device, specs[k]!, dir, `${k}.dkm`)));
      const needs = await needsOf(files);
      const rt = await rig.load('fits', {
        modules: names.map((k, i) => ({ name: `modules/${k}.dkm`, file: files[i]! })),
        selection: [],
      });
      const { session } = rt;
      const cap = await rig.usb(rt, (dk) => dk.capacity());
      const at = (symbol: string) => (symbol === 'TICK' ? menuSymbol(rig.plan, symbol) : planSymbol(rig.plan, symbol));
      const peek = async (address: number) => (await session.client.memPeek(address))! >>> 0;
      const text = async (symbol: string, i = 0, stride = 24) => {
        const bytes = Buffer.alloc(stride);
        for (let k = 0; k < stride; k += 4) bytes.writeUInt32BE(await peek(at(symbol) + stride * i + k), k);
        return bytes.toString('latin1').split('\0')[0]!;
      };
      const rows = async () => Promise.all(Array.from({ length: await peek(at('DK_LOAD_N')) }, (_, i) => text('DK_LOAD', i)));
      /** TICK: 1 for each row that is ticked. */
      const ticked = async () => {
        const v = await peek(at('TICK'));
        return names.map((_, k) => (v >>> (24 - 8 * k)) & 0xff);
      };
      const dialog = async () => [await text('DK_POPUP', 0, 48), await text('DK_POPUP_TEXT', 0, 48)];
      const press = (key: string, settle = 300) => tap(session, key, settle);
      const presses = async (key: string, n: number) => {
        for (let i = 0; i < n; i++) await press(key, 200);
      };
      const [first, second, huge] = ['half-a', 'half-b', 'huge'].map((k) => names.indexOf(k)) as [number, number, number];
      const want = names.map((k, i) => pageRow(`${k}.dkm`, needs[i]!));

      // SETTINGS > DIGIKIT (the ninth row) > MODULES (the fourth row)
      await openDigikitMenu(async (name, settle) => press(name, Math.min(settle ?? 300, 800)));
      await presses('DOWN', 3);
      await press('YES', 1500);
      await expect.poll(async () => (await advance(session, 100), await peek(at('DK_LOAD_N'))), { timeout: 60_000, interval: 10 }).toBe(names.length);
      expect({ rows: await rows(), ticked: await ticked() }, 'a row for each file with its name, kind and the memory it takes; nothing is ticked').toEqual({
        rows: want,
        ticked: names.map(() => 0),
      });

      // YES on the module larger than the DSP area: refused
      const popups = await peek(at('DK_POPUPS'));
      await presses('DOWN', huge);
      await press('YES', 700);
      const hugeWhy = (await fit(cap, [needs[huge]!], true)).each[0];
      expect(
        [await peek(at('DK_POPUPS')), await ticked(), await dialog()],
        'YES on the module larger than the DSP area ticks nothing and opens a confirm with the reason',
      ).toEqual([popups + 1, names.map(() => 0), ['HUGE DOES NOT FIT', hugeWhy.text]]);
      await press('YES', 500); // the confirm is a notice

      // one half-size module ticked: it fits, and nothing else changes
      await presses('UP', huge);
      await presses('DOWN', first);
      await press('YES', 500);
      const onlyFirst = names.map((_, i) => Number(i === first));
      expect({ ticked: await ticked(), rows: await rows() }, 'one half-size module ticked: it is fine, and nothing else changed').toEqual({
        ticked: onlyFirst,
        rows: want,
      });

      // the other half-size module next to it is refused: the mark stays off and the confirm says which resource is short
      await presses('DOWN', second - first);
      const popups2 = await peek(at('DK_POPUPS'));
      await press('YES', 700);
      const setWhy = (await fit(cap, [needs[first]!, needs[second]!], true)).set;
      expect(
        [await peek(at('DK_POPUPS')), await ticked(), await dialog()],
        'the other half-size module is refused: the mark stays off and the confirm says which resource is short',
      ).toEqual([popups2 + 1, onlyFirst, ['HALF-B DOES NOT FIT', setWhy.text]]);
      await press('YES', 500);

      // untick the first: there is room for the second again
      await presses('UP', second - first);
      await press('YES', 500);
      await presses('DOWN', second - first);
      const popups3 = await peek(at('DK_POPUPS'));
      await press('YES', 500);
      expect(
        [await ticked(), await peek(at('DK_POPUPS'))],
        'unticking the first module makes room: the second can be ticked',
      ).toEqual([names.map((_, i) => Number(i === second)), popups3]);
      await rpc(session, 'run.mode', { mode: 'paused' });
    },
  );
});
