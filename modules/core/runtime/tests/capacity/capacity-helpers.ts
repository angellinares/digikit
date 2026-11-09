import { expect } from 'vitest';
import { bootInfo, E_LOADING } from '@digi/regression/runtime/card-kit.ts';
import { CARD_SYMBOLS, RT, type CardRig } from '@digi/regression/runtime/card-rig.ts';
import { word } from '@digi/regression/runtime/fx-live.ts';
import { ringEvents } from '@digi/regression/runtime/runtime-menu.ts';
import { planSymbol } from '@digi/regression/runtime/runtime-plan.ts';
import { LOADED, type Geometry, type Want } from '@digi/regression/runtime/synth.ts';

/** One module of a card in boot order: its file, the name it reports and the result the loader must give. */
export interface Entry {
  name: string;
  file: string;
  want: Want;
}

export const SYMBOLS = [...CARD_SYMBOLS, 'DKRT_ARENA', '_dl_area', 'DKRT_TABLE_BAD'];
export const popcount = (n: number) => n.toString(2).replace(/0/g, '').length;
const resultOk = (got: number, want: Want) => (want === LOADED ? got === 0 || got === E_LOADING : got === want);

/** Boot a card of ENTRIES (selected in order) and read what the runtime says. */
export async function bootEntries(rig: CardRig, tag: string, entries: Entry[]) {
  const rt = await rig.load(tag, {
    modules: entries.map((e, i) => ({ name: `modules/m${i}.dkm`, file: e.file })),
    selection: entries.map((_, i) => `modules/m${i}.dkm`),
  });
  const info = await bootInfo(rt.session, rig.plan);
  const { mods, cap } = await rig.usb(rt, async (dk) => ({ mods: await dk.modules(), cap: await dk.capacity() }));
  return { rt, info, mods, cap };
}

/** Every nonzero range [off, off + len) lies in [lo, hi) and no two overlap. */
function rangesOk(ranges: [number, number][], lo: number, hi: number): boolean {
  const rs = ranges
    .filter(([, len]) => len)
    .map(([off, len]) => [off, off + len] as const)
    .sort((a, b) => a[0] - b[0]);
  return rs.every(([a, b]) => lo <= a && b <= hi) && rs.every((r, i) => i === rs.length - 1 || r[1] <= rs[i + 1]![0]);
}

/** The checks every boot of a set shares. Returns the loaded modules. */
export async function expectBoot(
  rig: CardRig,
  g: Geometry,
  entries: Entry[],
  got: Awaited<ReturnType<typeof bootEntries>>,
) {
  const { rt, info, mods, cap } = got;
  expect(info.count, 'the boot saw every selected module').toBe(entries.length);
  entries.forEach((e, i) =>
    expect(resultOk(info.results[i]!, e.want), `${e.name}: result ${info.results[i]}, wanted ${e.want}`).toBe(true),
  );
  const loaded = entries.filter((e) => e.want === LOADED).map((e) => e.name);
  expect(
    mods.map((m) => m.name),
    'MODULES lists exactly the loaded modules, in order',
  ).toEqual(loaded);
  expect(
    rangesOk(
      mods.map((m) => [m.cf_base, m.cf_size]),
      g.arenaLo,
      g.arenaHi,
    ),
    'ColdFire blocks lie in the arena and do not overlap',
  ).toBe(true);
  expect(
    rangesOk(
      mods.map((m) => [m.dsp_off, m.dsp_len]),
      0,
      g.dspArea,
    ),
    'DSP ranges lie in the area and do not overlap',
  ).toBe(true);
  const rows = mods.reduce((n, m) => n | m.rows, 0);
  expect(cap.MODULES!, 'CAPACITY modules').toMatchObject({ limit: g.modules, used: loaded.length });
  expect(cap.SELECTION!.used, 'CAPACITY selection').toBe(entries.length);
  expect(cap.ARENA!.limit, 'CAPACITY arena').toBe(g.arenaFree);
  expect(cap.STAGE!, 'CAPACITY stage').toMatchObject({ limit: g.stageBytes, largest: g.stageBytes });
  expect(cap.DSP_AREA!.limit, 'CAPACITY DSP area').toBe(g.dspArea);
  expect(cap.VOICE_ROWS!, 'CAPACITY voice rows are the owned rows').toMatchObject({
    limit: g.machineRows,
    used: popcount(rows),
  });
  expect([cap.MACHINE_ROWS!.limit, cap.PARAM_ROWS!.limit, cap.PAGE_ROWS!.limit], 'CAPACITY registry rows').toEqual([
    g.machineRows,
    g.paramRows,
    g.pageRows,
  ]);
  const bad = (await word(rt.session, planSymbol(rig.plan, 'DKRT_TABLE_BAD'))) >>> 24;
  expect(
    [bad, (await ringEvents(rt.session, rig.plan, 'TABLE_BAD')).length],
    'the table check never failed: a refused module left nothing behind',
  ).toEqual([0, 0]);
  return mods;
}

/** Core adapters needed by loaded modules that import from them or take rows. */
export const COUPLED_ROOTS = [...RT, 'core/input', 'core/dynfx', 'core/fxpages'];
