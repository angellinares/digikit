import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { afterAll, beforeAll, describe, expect, it } from 'vitest';
import { f2w } from '../src/context.ts';
import { fxSpec, fxSpecs, frameHw, fxWord, galleryGaps, machineSpec, machineSpecs, paramMid } from '../src/spec.ts';
import { param } from './fakes.ts';

let root: string;
const write = (file: string, text: string) => {
  mkdirSync(path.dirname(path.join(root, file)), { recursive: true });
  writeFileSync(path.join(root, file), text);
};

beforeAll(() => {
  root = mkdtempSync(path.join(tmpdir(), 'rig-spec-'));
  process.env.PATCHKIT_MODULES = root;
  write(
    'zz/module.toml',
    `[module]
id = "zzt/mach"
type = "machine"
category = "machines"
dynamic = true
ports = ["dt2-1.17"]

[[machine]]
id = "mach-one"
dsp = "mo"

[[object]]
path = "build/dsp.dko.json"
sources = ["src/dsp/mo.s", "src/module.c"]
devices = ["dt2"]

[conformance]
fixtures = ["sample-pool"]
sample_records = "_mo_pool"
`,
  );
  write(
    'zz/build/gen/gallery.json',
    JSON.stringify({
      machines: [{ devices: { dt2: { knobs: [{ param: 'decay' }, { stock: 202 }, { param: 'unknown' }, 'x'] } } }],
      params: { dt2: [{ id: 'decay', min: 16, max: 1000, default: 500, latched_word: 2 }] },
    }),
  );
  write(
    'fx/module.toml',
    `[module]
id = "zzt/fx"
type = "fx"
category = "fx"

[[fx]]
id = "wob-ble"

[[object]]
path = "build/fx.dko.json"
sources = ["src/dsp/wob.s"]
`,
  );
  write(
    'fx/build/gen/gallery.json',
    JSON.stringify({
      effects: [
        {
          knobs: [
            { min: 0, max: 10 },
            { min: -1, max: 1 },
          ],
        },
      ],
    }),
  );
  write(
    'gap/module.toml',
    `[module]
id = "zzt/gap"
type = "machine"
category = "machines"
dynamic = true
ports = ["dt2-1.17", "dn2-1.12"]

[targets]
"dt2-1.17" = "experimental"
"dn2-1.12" = "experimental"

[[machine]]
id = "gap-one"
dsp = "go"

[[object]]
path = "build/dsp.dko.json"
sources = ["src/dsp/go.s"]
`,
  );
  write(
    'gap/build/gen/gallery.json',
    JSON.stringify({
      machines: [{ id: 'gap-one', devices: { dn2: { knobs: [] }, dt2: { knobs: [] } } }],
      params: { dn2: [{ id: 'x' }], dt2: [{ id: 'x' }] },
    }),
  );
});

afterAll(() => {
  delete process.env.PATCHKIT_MODULES;
  rmSync(root, { recursive: true });
});

describe('machineSpec', () => {
  it('reads the symbols, the DSP sources, the parameters with their knob slots and the type of a run-time machine', () => {
    const s = machineSpec('zzt/mach', 'dt2')!;
    expect(s.entry).toBe('_mo_render');
    expect([s.noteOn, s.desc]).toEqual(['_mo_note_on', '_mo_desc']);
    expect(s.sources).toEqual([path.join(root, 'zz/src/dsp/mo.s')]); // the C source of the object is no DSP source
    expect(s.params).toEqual([
      { id: 'decay', knob: 0, lo: 16, hi: 1000, default: 500, stock: false, latchedWord: 2 },
      { id: 'tune', knob: 1, lo: 0, hi: 0x7f00, default: 0x4000, stock: true, latchedWord: null }, // the stock TUNE knob
    ]);
    expect(s.machineType).toBe(16); // a run-time machine takes the first number of the run-time range
    expect([s.fixtures, s.sampleRecords]).toEqual([['sample-pool'], '_mo_pool']);
    expect(s.frame.base).toBe(0x6d);
  });

  it('is null for a device the module does not port to, and for a module that is no machine', () => {
    expect(machineSpec('zzt/mach', 'dn2')).toBeNull();
    expect(machineSpec('zzt/fx', 'dt2')).toBeNull();
  });

  it('lists the machines of the module roots, the negative fixtures apart', () => {
    expect(machineSpecs().map((s) => `${s.module}@${s.device}`)).toContain('zzt/mach@dt2');
    expect(machineSpecs(true).map((s) => s.module)).not.toContain('zzt/mach');
    expect(machineSpecs(true).map((s) => s.module)).toContain('test/dyn-guard-nan#overrun');
  });
});

describe('galleryGaps', () => {
  const gallery = (machines: unknown[], params: unknown) =>
    write('gap/build/gen/gallery.json', JSON.stringify({ machines, params }));
  const two = { dn2: { knobs: [] }, dt2: { knobs: [] } };
  const filled = { dn2: [{ id: 'x' }], dt2: [{ id: 'x' }] };

  it('names a device of [targets] that the gallery does not list', () => {
    gallery([{ id: 'gap-one', devices: { dn2: { knobs: [] } } }], filled);
    expect(galleryGaps('zzt/gap')).toEqual(['dt2: machine gap-one is not listed in build/gen/gallery.json']);
  });

  it('names a device entry with an empty parameter list', () => {
    gallery([{ id: 'gap-one', devices: two }], { dn2: [{ id: 'x' }], dt2: [] });
    expect(galleryGaps('zzt/gap')).toEqual(['dt2: machine gap-one has no parameter in build/gen/gallery.json']);
  });

  it('checks every machine of the gallery, not only the first', () => {
    gallery(
      [
        { id: 'gap-one', devices: two },
        { id: 'gap-two', devices: { dn2: { knobs: [] } } },
      ],
      filled,
    );
    expect(galleryGaps('zzt/gap')).toEqual(['dt2: machine gap-two is not listed in build/gen/gallery.json']);
  });

  it('reports nothing for a complete gallery', () => {
    gallery([{ id: 'gap-one', devices: two }], filled);
    expect(galleryGaps('zzt/gap')).toEqual([]);
  });
});

describe('fxSpec', () => {
  it('reads a run-time effect: the process and description symbols and the range of each page knob', () => {
    const s = fxSpec('zzt/fx')!;
    expect([s.process, s.desc, s.state]).toEqual(['_wob_ble_process', '_wob_ble_desc', '']);
    expect(s.knobs).toEqual([
      [0, 10],
      [-1, 1],
    ]);
    expect(fxSpecs().map((x) => x.module)).toContain('zzt/fx');
    expect(fxSpecs(true).map((x) => x.module)).not.toContain('zzt/fx');
  });

  it('turns a case value into the float the runtime sends for a knob of a page, and leaves the other words raw', () => {
    const s = fxSpec('zzt/fx')!;
    expect(fxWord(s, 0, 0x7f00)).toBe(f2w(10));
    expect(fxWord(s, 1, 0)).toBe(f2w(-1));
    expect(fxWord(s, 1, 0x3f80)).toBe(f2w(Math.fround(-1 + 2 * (0x3f80 / 0x7f00))));
    expect(fxWord(s, 2, 1234)).toBe(1234);
  });
});

describe('frame and parameter helpers', () => {
  it('finds the frame halfword of knob j of voice v', () => {
    const frame = { base: 0x6d, stride: 48, group: 4, gap: 2 };
    expect(frameHw(frame, 0, 0)).toBe(0x6d);
    expect(frameHw(frame, 0, 4)).toBe(0x6d + 4 + 2); // the second group starts two halfwords later
    expect(frameHw(frame, 2, 5)).toBe(0x6d + 96 + 5 + 2);
    expect(frameHw({ base: 1, stride: 10, group: 0, gap: 7 }, 1, 3)).toBe(14); // no groups: no gap
  });

  it('puts the mid word of a parameter on the grid of words that are UI values times 256', () => {
    expect(paramMid(param('x', 0, 0, 0x7f00))).toBe(0x3f00);
    expect(paramMid(param('x', 0, 1, 1000))).toBe(0x100);
  });
});
