import fs from 'node:fs';
import path from 'node:path';
import { moduleStateText, type ModuleState } from '@digi/dk-session';
import { CAPACITY_KINDS, RECORD_KIND, RESOURCE } from '@digi/dk-session/gen_proto';
import { expect, test } from 'vitest';
import { cardRigs, type CardRig } from '@digi/regression/runtime/card-rig.ts';
import { moduleArtifact } from '@digi/regression/runtime/install.ts';
import { loadCode, stateName } from '@digi/regression/runtime/load-states.ts';
import { LOADED } from '@digi/regression/runtime/synth.ts';
import { bootEntries, COUPLED_ROOTS, SYMBOLS, type Entry } from '../capacity/capacity-helpers.ts';
import { edits, patched, unhashed } from './dkm-patch.ts';

/**
 * Refusal fixtures for the loader and the boot plan, with module states (docs/plans/runtime-and-services.md section 8, item R11; the table of codes is
 * `refusal-table.ts`). Each fixture is a copy of the packed `test/commit-demo` with one field changed (`dkm-patch.ts`) or a pair of lfo fixtures, so a must-fail
 * differs from the must-pass (the unchanged module) by that field only. The test reads three views of one boot and they must agree:
 *
 * - `DKRT_BOOT` (`results`, `states`, `detail`): the module's state and the code, read from guest memory;
 * - the LOADS command (version 2): the same record through the protocol, as Transfer reads it;
 * - MODULES: only the module that is ACTIVE is listed, so a refused or failed module left nothing behind.
 *
 * The boot runs without the SHARC (`dsp: 'cold'`): the good module's DSP part stays pending, so its state is DSP_LOADING there and ACTIVE with the SHARC.
 */
const GOOD = 'test/commit-demo';
const LFO = 'test/lfo-shape';
const LFO_DUP = 'test/lfo-shape-dup';
/** The service of the lfo_shape kind is a file of the card (`services/lfo.dks`), not a part of the image: the two fixtures need it. */
const LFO_SERVICE = 'core/services/lfo';
const UNOWNED_KIND = 90;

interface Seen {
  results: number[];
  states: string[];
  detail: [number, number][];
  loads: ModuleState[];
  modules: string[];
}

/** One boot of `entries`, and everything the tests read from it as plain values (the tests do not touch the device again). */
async function boot(rig: CardRig, tag: string, entries: Entry[], files: { name: string; file: string }[] = []): Promise<Seen> {
  const got = await bootEntries(rig, tag, entries, files);
  const loads = await rig.usb(got.rt, (dk) => dk.loadStates());
  return {
    results: got.info.results,
    states: got.info.states.map(stateName),
    detail: got.info.detail,
    loads,
    modules: got.mods.map((m) => m.name),
  };
}

/** The state, code and detail of module `i` as the boot record and the LOADS reply both say them. */
function expectModule(seen: Seen, i: number, state: string, code: number, detail?: [number, number]) {
  const at = `module ${i}`;
  expect([seen.states[i], seen.results[i]], `${at} in DKRT_BOOT`).toEqual([state, code]);
  expect([seen.loads[i]?.state, seen.loads[i]?.result, seen.loads[i]?.index], `${at} in LOADS`).toEqual([state, code, i]);
  if (detail) {
    expect(seen.detail[i], `${at}: what the plan refused on, in DKRT_BOOT`).toEqual(detail);
    expect(seen.loads[i]!.detail, `${at}: and in LOADS`).toEqual(detail);
  }
}

/** Each module loads or is ACTIVE, or DSP_LOADING while its DSP part is going (no SHARC here). */
function expectLoaded(seen: Seen, i: number) {
  expect(['ACTIVE', 'DSP_LOADING'], `module ${i} state`).toContain(seen.states[i]);
  expect([0, 1], `module ${i} result: OK or LOADING`).toContain(seen.results[i]);
  expect(seen.loads[i]!.state).toBe(seen.states[i]);
  expect(moduleStateText(seen.loads[i]!), 'a loaded module has no refusal text').toBeUndefined();
}

cardRigs('refusal fixtures and module states', { dsp: 'cold', roots: COUPLED_ROOTS, symbols: SYMBOLS }, (rig) => {
  const fixture = () => fs.readFileSync(moduleArtifact(rig.device, GOOD));
  const entryOf = (name: string, f: Buffer, want: Entry['want']): Entry => {
    const dir = path.join(rig.artifacts, 'refusal');
    fs.mkdirSync(dir, { recursive: true });
    const file = path.join(dir, `${name.split('/').at(-1)}.dkm`);
    fs.writeFileSync(file, f);
    return { name, file, want };
  };

  // one boot for all the patched copies of the good module: entry 0 is the good one
  let patchedBoot: Promise<Seen> | undefined;
  const patchedSeen = (): Promise<Seen> => {
    patchedBoot ??= (async () => {
      await rig.first();
      const E_FORMAT = loadCode('E_FORMAT');
      const entries: Entry[] = [
        { name: GOOD, file: moduleArtifact(rig.device, GOOD), want: LOADED },
        entryOf('test/c-format', patched(fixture(), 'test/c-format', edits.badMagic), E_FORMAT),
        entryOf('test/c-sha', unhashed(patched(fixture(), 'test/c-sha')), loadCode('E_SHA')),
        entryOf('test/c-newer', patched(fixture(), 'test/c-newer', edits.newerRuntime), loadCode('E_CONTRACT')),
        entryOf('test/c-need', patched(fixture(), 'test/c-need', edits.needsNewerRuntime), loadCode('E_CONTRACT')),
        entryOf('test/c-missing', patched(fixture(), 'test/c-missing', edits.needsMissingService), loadCode('E_MISSING')),
        entryOf('test/c-kind', patched(fixture(), 'test/c-kind', edits.unownedKind(UNOWNED_KIND)), loadCode('E_PLAN_KIND')),
        entryOf('test/c-fit', patched(fixture(), 'test/c-fit', edits.machineRows(RESOURCE.MACHINE_ROWS, 99)), loadCode('E_PLAN_FIT')),
      ];
      return boot(rig, 'refusal-patched', entries);
    })();
    return patchedBoot;
  };

  test('the good module loads and is ACTIVE', async () => {
    const seen = await patchedSeen();
    expectLoaded(seen, 0);
    expect(seen.modules, 'MODULES lists the good module and no refused or failed copy').toEqual([GOOD]);
  });

  test('a wrong magic is E_FORMAT: the plan accepts it, the loader fails it', async () => {
    const seen = await patchedSeen();
    expectModule(seen, 1, 'FAILED', loadCode('E_FORMAT'));
    expect(moduleStateText(seen.loads[1]!)).toMatch(/^test\/c-format not loaded|^c-format\.dkm not loaded|not loaded: bad magic/);
  });

  test('a changed sha256 is E_SHA', async () => {
    expectModule(await patchedSeen(), 2, 'FAILED', loadCode('E_SHA'));
  });

  test('a module that needs a newer runtime is E_CONTRACT: FAILED when the header says it, REFUSED when the need table says it', async () => {
    const seen = await patchedSeen();
    expectModule(seen, 3, 'FAILED', loadCode('E_CONTRACT'));
    expectModule(seen, 4, 'REFUSED', loadCode('E_CONTRACT'));
    expect(moduleStateText(seen.loads[4]!)).toContain('refused: it needs a newer runtime than this one');
  });

  test('a hard need nobody provides is E_MISSING', async () => {
    const seen = await patchedSeen();
    expectModule(seen, 5, 'REFUSED', loadCode('E_MISSING'));
    expect(moduleStateText(seen.loads[5]!)).toContain('refused: the service 0xdeadbeef that it needs');
  });

  test('keys for a kind nobody owns are refused with E_PLAN_KIND and the kind', async () => {
    const seen = await patchedSeen();
    expectModule(seen, 6, 'REFUSED', loadCode('E_PLAN_KIND'), [UNOWNED_KIND, 0]);
    expect(moduleStateText(seen.loads[6]!)).toContain(`records of kind ${UNOWNED_KIND} have no owner`);
  });

  test('uses over the capacity are refused with E_PLAN_FIT and the resource that is full', async () => {
    const seen = await patchedSeen();
    expectModule(seen, 7, 'REFUSED', loadCode('E_PLAN_FIT'), [CAPACITY_KINDS.MACHINE_ROWS, 0]);
    expect(moduleStateText(seen.loads[7]!)).toContain('refused: no room: the machine rows');
  });

  // the two failures that come after the plan, each in a boot of its own copy of the good module (they share its machine key and DSP part, so the good module is not in this boot)
  let failures: Promise<Seen> | undefined;
  const failuresSeen = (): Promise<Seen> => {
    failures ??= (async () => {
      await rig.first();
      const entries: Entry[] = [
        entryOf('test/c-init', patched(fixture(), 'test/c-init', edits.failInit), loadCode('E_INIT')),
        entryOf('test/c-decl', patched(fixture(), 'test/c-decl', edits.undeclaredKinds), loadCode('E_DECL_KIND')),
      ];
      return boot(rig, 'refusal-failures', entries);
    })();
    return failures;
  };

  test('an init that returns non-zero is E_INIT: the plan accepts the module, the load fails and leaves nothing', async () => {
    const seen = await failuresSeen();
    expectModule(seen, 0, 'FAILED', loadCode('E_INIT'));
    expect(moduleStateText(seen.loads[0]!)).toContain('not loaded: init returned non-zero');
    expect(seen.modules, 'MODULES lists neither failed module').toEqual([]);
  });

  test('kinds in the header table that init does not declare are E_DECL_KIND', async () => {
    const seen = await failuresSeen();
    expectModule(seen, 1, 'FAILED', loadCode('E_DECL_KIND'));
    expect(moduleStateText(seen.loads[1]!)).toContain('not loaded: a service declared a record kind');
  });

  test('a selected file the card does not have is E_NOT_FOUND', async () => {
    await rig.first();
    const rt = await rig.load('refusal-gone', {
      modules: [{ name: 'modules/m0.dkm', file: moduleArtifact(rig.device, GOOD) }],
      selection: ['modules/m0.dkm', 'modules/gone.dkm'],
    });
    const loads = await rig.usb(rt, (dk) => dk.loadStates());
    expect(loads.map((m) => [m.state, m.result]).slice(1), 'the missing file: FAILED with E_NOT_FOUND').toEqual([['FAILED', loadCode('E_NOT_FOUND')]]);
    expect(['ACTIVE', 'DSP_LOADING']).toContain(loads[0]!.state);
  });

  let owners: Promise<Seen> | undefined;
  const ownersSeen = (): Promise<Seen> => {
    owners ??= (async () => {
      await rig.first();
      const entries: Entry[] = [
        { name: LFO, file: moduleArtifact(rig.device, LFO), want: LOADED },
        { name: LFO_DUP, file: moduleArtifact(rig.device, LFO_DUP), want: loadCode('E_PLAN_KEY') },
      ];
      return boot(rig, 'refusal-owners', entries, [{ name: 'services/lfo.dks', file: moduleArtifact(rig.device, LFO_SERVICE) }]);
    })();
    return owners;
  };

  test('the first owner of the key loads', async () => {
    const seen = await ownersSeen();
    expectLoaded(seen, 0);
    expect(seen.modules).toEqual([LFO]);
  });

  test('a second owner of one key is E_PLAN_KEY, with the kind and the key', async () => {
    const seen = await ownersSeen();
    expectModule(seen, 1, 'REFUSED', loadCode('E_PLAN_KEY'));
    expect(seen.detail[1]![0], 'the kind: lfo_shape').toBe(RECORD_KIND.LFO_SHAPE);
    expect(seen.loads[1]!.detail, 'LOADS carries the same words').toEqual(seen.detail[1]);
    expect(moduleStateText(seen.loads[1]!)).toContain(`for record kind ${RECORD_KIND.LFO_SHAPE}`);
  });
});
