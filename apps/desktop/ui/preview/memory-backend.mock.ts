// The Memory panel's backend in the preview (replaces ../src/memory-backend.ts): CAPACITY and MODULES of a fake device, built from the library of
// sim.ts. The state is in the address: `card=dn2|dt2` (the device: the Digitakt II has no page rows), `mem=pluck|full|empty|dsp` (what is loaded):
//   pluck  Pluck and Remote control, and one module that is not in the library (the default)
//   full   a machine pair, an effect, a tool and the unknown module
//   empty  nothing loaded
//   dsp    Pluck, Wavetable and FM 4-op: the DSP memory is nearly full
//   split  as pluck, with the free CPU memory split in blocks (largest block 90 KiB)
//   over   as dsp, with the DSP memory used above its limit
// The figures go through the app's own `buildBars`, so the picture is what a device with these modules shows.
import type { Caps } from '@web/modules/fit';
import { buildBars, type KnownModule } from '../src/memory-core';
import { PAUSED } from '../src/desktop-env';
import { LIBRARY } from './sim';

export { PAUSED };
import type { MemoryReading } from '../src/memory-backend';

const query = new URLSearchParams(location.search);
const dev = query.get('card') === 'dt2' ? 'dt2' : 'dn2';
const mem = query.get('mem') ?? 'pluck';
const LOADED: Record<string, string[]> = {
  pluck: ['pluck', 'remote', 'someone/mystery'],
  full: ['sine', 'pluck', 'chorus', 'remote', 'someone/mystery'],
  empty: [],
  dsp: ['pluck', 'wavetable', 'fm4'],
  split: ['pluck', 'remote', 'someone/mystery'],
  over: ['pluck', 'wavetable', 'fm4'],
};
const KiB = 1024;

export async function readMemory(): Promise<MemoryReading> {
  const names = LOADED[mem] ?? [];
  const known: KnownModule[] = LIBRARY.map((m) => ({ name: m.id, label: m.label, group: m.group, needs: m.needs }));
  const used = { ARENA: 0, DSP_AREA: 0, MACHINE_ROWS: 0, PARAM_ROWS: 0, PAGE_ROWS: 0 };
  for (const n of names) {
    const m = LIBRARY.find((l) => l.id === n);
    // a module that is not in the library takes arena, DSP memory and rows of its own (the panel shows them as "other")
    const needs = m?.needs ?? { ARENA: 12 * KiB, DSP_AREA: 40 * KiB, MACHINE_ROWS: 1, PARAM_ROWS: 2 };
    used.ARENA += needs.ARENA ?? 0;
    used.DSP_AREA += needs.DSP_AREA ?? 0;
    used.MACHINE_ROWS += needs.MACHINE_ROWS ?? 0;
    used.PARAM_ROWS += needs.PARAM_ROWS ?? 0;
  }
  if (names.length) used.ARENA += 6 * KiB; // the runtime's own blocks
  const c = (limit: number, u: number) => ({ limit, used: u, largest: limit - u });
  const caps: Caps = {
    MODULES: c(8, names.length),
    ARENA: c(512 * KiB, used.ARENA),
    DSP_AREA: c(512 * KiB, used.DSP_AREA),
    MACHINE_ROWS: c(8, used.MACHINE_ROWS),
    PARAM_ROWS: c(16, used.PARAM_ROWS),
    PAGE_ROWS: c(dev === 'dn2' ? 8 : 0, 0),
  };
  if (mem === 'split') caps.ARENA.largest = 90 * KiB;
  if (mem === 'over') caps.DSP_AREA.used += 80 * KiB;
  await new Promise((r) => setTimeout(r, 120));
  return { port: dev === 'dn2' ? 'dn2-1.12' : 'dt2-1.17', bars: buildBars(caps, names, known), loaded: names.length };
}
