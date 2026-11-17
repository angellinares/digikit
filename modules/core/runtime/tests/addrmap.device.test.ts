import fs from 'node:fs';
import path from 'node:path';
import { expect } from 'vitest';
import { liveArtifacts } from '@digi/regression/runtime/artifacts.ts';
import { resolveAddresses, readLoadTable } from '@digi/regression/runtime/addrmap.ts';
import { DEBUG_MODULE, installModules, moduleArtifact } from '@digi/regression/runtime/install.ts';
import { expectRuntimeReady, runtimeTests } from '@digi/regression/runtime/ready.ts';
import { planSymbol, prepareRuntimePlan } from '@digi/regression/runtime/runtime-plan.ts';

/**
 * Address map (docs/ADDRMAP.md), emulator only: after a boot with one run-time module, the load table read from the runtime's module table names a
 * function of the runtime (layer our, from the ship map `patchkit apply --addrmap` wrote for this image) and a function of the module (the
 * module's own map, rebased by its load address): the module's `fini` address, which the runtime keeps in the same record, is the start of
 * `dyn_demo::fini`.
 */
const ID = 'test/dyn-demo';

runtimeTests('the address map names a runtime function and a loaded module function', async (runtime) => {
  const { session, dk } = runtime;
  await expectRuntimeReady(session);
  const device = session.options.device;
  const plan = await prepareRuntimePlan(session, liveArtifacts(session, 'addrmap'), ['DKRT_STATE', 'DKRT_DSP', 'dyndsp_info']);
  const shipMap = path.join(session.runDir, `runtime-${device}.addrmap.json`);
  const artifact = moduleArtifact(device, ID);
  const moduleMap = artifact.replace(/\.dkm$/, '.addrmap.json');
  expect(fs.existsSync(moduleMap), `${moduleMap}: run nx run ${ID}:pack`).toBe(true);
  // the debug module last: the boot wait reads its log
  await installModules(dk, [
    { id: ID, artifact },
    { id: DEBUG_MODULE, artifact: moduleArtifact(device, DEBUG_MODULE) },
  ], { device });

  const loads = await readLoadTable(session, plan, shipMap);
  expect(loads.map((l) => l.id)).toEqual([ID, DEBUG_MODULE]);
  const load = loads[0]!;
  expect(
    loads.map((l) => l.dspBase),
    'dyn-demo and the debug module have no DSP part',
  ).toEqual([undefined, undefined]);
  expect(load.fini, 'the module has a fini function').toBeGreaterThan(load.base);

  const runtimeFn = planSymbol(plan, 'dkrt_module');
  const [inRuntime, inModule, inside] = await resolveAddresses(
    [shipMap, moduleMap],
    loads,
    [runtimeFn, load.fini, load.fini + 4],
  );
  expect(inRuntime, 'a runtime function by name').toMatchObject({ name: 'dkrt_module', layer: 'our', offset: 0, module: null });
  expect(inModule, "the module's fini by name, rebased by the load address").toMatchObject({
    name: 'dyn_demo::fini',
    layer: 'our',
    offset: 0,
    module: ID,
    sym_addr: load.fini,
  });
  expect(inside, 'inside the function').toMatchObject({ text: 'dyn_demo::fini+0x4' });
  const [other] = await resolveAddresses([shipMap, moduleMap], [{ id: ID, base: load.base + 0x1000 }], [load.fini]);
  expect(other!.name, 'a wrong load address does not name the module function').not.toBe('dyn_demo::fini');
});
