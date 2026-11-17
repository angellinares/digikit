// The merged module list of the Transfer page (`mergeView`, `libraryModule`, `headerInfo` in dktransfer.ts) with the real fit rule of patchkit.wasm: what fits,
// the warnings with their texts, the load failures. The rest of the Transfer logic runs in dktransfer.test.ts with a fit stand-in.
import assert from 'node:assert/strict';
import { test } from 'vitest';
import type { ModuleState } from '@digi/dk-session';
import { headerInfo, libraryModule, mergeView } from '@digi/workbench-core/modules/dktransfer.ts';
import { hex, sha } from '@digi/test-support/fakedevice';
import { dkm, needsOf } from './support/dkm.ts';
import { useFitWasm } from './fit-wasm.ts';

await useFitWasm();

test('header, library module, the merged list (the real fit rule)', async () => {
  const mach = dkm('someone/machine', { machine: 1, dsp: 64, size: 900 });
  const sine = dkm('test/sine', { machine: 1, dsp: 400, size: 800 });
  const tool = dkm('someone/tool', { size: 500 });
  assert.deepEqual([headerInfo(mach)?.group, headerInfo(mach)?.version], ['Machines', '0.1.2']);
  assert.equal(headerInfo(dkm('a', { fx: 1 }))?.group, 'Effects');
  assert.equal(headerInfo(tool)?.group, 'Tools');
  assert.equal(headerInfo(new Uint8Array(300)), null);
  assert.equal(await libraryModule(new Uint8Array(300)), null);
  const lm = (await libraryModule(mach, needsOf))!;
  assert.deepEqual(
    [lm.id, lm.label, lm.test, lm.path, lm.file_size, lm.hash],
    ['someone/machine', 'machine', false, 'modules/machine.dkm', 900, hex(sha(mach))],
  );
  assert.deepEqual([lm.needs?.ARENA, lm.needs?.DSP_AREA, lm.needs?.file_size], [120, 64, 900]);
  // no reader, or a file the reader says the loader would refuse: no needs, so no fit verdict (the device still refuses what does not load)
  assert.equal((await libraryModule(mach))?.needs, null);
  assert.equal((await libraryModule(mach, () => null))?.needs, null);
  const unknown = mergeView(
    [(await libraryModule(mach, () => null))!],
    [],
    [],
    { MODULES: { limit: 8, used: 8, largest: 0 } },
    {},
  );
  assert.deepEqual([unknown.modules[0].warnings, unknown.modules[0].needs], [[], undefined]);

  const library = [(await libraryModule(sine, needsOf))!, lm, (await libraryModule(tool, needsOf))!];
  const card = [
    { path: 'modules/machine.dkm', size: 900, hash: hex(sha(mach)) }, // same file
    { path: 'modules/tool.dkm', size: 500, hash: 'ff' }, // same path, other bytes
    { path: 'modules/other.dkm', size: 700, hash: 'aa' }, // not from the library
    { path: 'modules/junk.dkm', size: 9, hash: 'bb' },
  ];
  const headers: Record<string, Uint8Array | null> = {
    'modules/other.dkm': dkm('someone/effect', { fx: 1, size: 700 }),
    'modules/junk.dkm': null,
  };
  // the device has 1000 bytes of DSP area, 50 of them in one free piece (a loaded module sits in the rest)
  const cap = {
    MODULES: { limit: 8, used: 1, largest: 0 },
    ARENA: { limit: 1000, used: 0, largest: 300 },
    DSP_AREA: { limit: 1000, used: 0, largest: 50 },
  };
  const v = mergeView(library, card, ['someone/machine', 'someone/effect'], cap, headers);
  assert.deepEqual(
    v.modules.map((r) => r.id),
    ['someone/machine', 'test/sine', 'someone/tool'],
  ); // group, test last, id
  const by = Object.fromEntries(v.modules.map((r) => [r.id, r]));
  assert.deepEqual(
    [by['someone/machine'].on_card, by['someone/machine'].loaded, by['someone/machine'].fits],
    ['yes', true, true],
  );
  assert.equal(by['someone/tool'].on_card, 'other version');
  assert.deepEqual([by['test/sine'].on_card, by['test/sine'].fits, by['test/sine'].test], ['no', false, true]); // DSP 400 > largest 50
  assert.match(
    by['test/sine'].warnings[0],
    /DSP memory 350 bytes short; fits when the modules loaded now are not selected/,
  );
  // a file the last boot selected and the loader refused says why; a loaded one and an unrelated one do not
  const failed: ModuleState[] = [
    { index: 0, name: 'tool.dkm', state: 'FAILED', result: -7, detail: [0, 0], needs: 'usb-bulk' },
  ];
  const why = mergeView(library, card, ['someone/machine', 'someone/effect'], cap, headers, failed);
  assert.equal(
    why.modules.find((r) => r.id === 'someone/tool')!.not_loaded,
    'tool not loaded: this firmware has no core/usb-bulk. Flash firmware from the Firmware page.',
  );
  assert.equal(why.modules.filter((r) => r.not_loaded).length, 1);
  assert.equal(
    mergeView(library, card, ['someone/tool'], cap, headers, failed).modules.find((r) => r.id === 'someone/tool')!
      .not_loaded,
    undefined,
  );
  // more than the device has at all: no way out by deselecting
  const huge = (await libraryModule(dkm('test/huge', { machine: 1, dsp: 2000, size: 800 }), needsOf))!;
  assert.match(
    mergeView([huge], [], [], cap, {}).modules[0]!.warnings![0],
    /DSP memory .* short; more than this device has/,
  );
  // a capacity without a kind (an older runtime) does not judge it
  assert.equal(mergeView(library, [], [], { MODULES: cap.MODULES }, {}).modules[1].fits, true);
  const extra = Object.fromEntries(v.card_only.map((r) => [r.path, r]));
  assert.deepEqual(
    [extra['modules/other.dkm'].group, extra['modules/other.dkm'].loaded, extra['modules/other.dkm'].name],
    ['Effects', true, 'someone/effect'],
  );
  assert.deepEqual([extra['modules/junk.dkm'].known, extra['modules/junk.dkm'].group], [false, 'Tools']);
  assert.equal(mergeView(library, [], [], null, {}).modules[0].fits, null); // no CAPACITY: no verdict
});
