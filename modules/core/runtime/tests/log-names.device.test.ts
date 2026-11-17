import fs from 'node:fs';
import path from 'node:path';
import { expect } from 'vitest';
import { DkProf } from '@digi/dk-session/dkprof';
import { liveArtifacts } from '@digi/regression/runtime/artifacts.ts';
import { DEBUG_MODULE, installModules, moduleArtifact } from '@digi/regression/runtime/install.ts';
import { expectRuntimeReady, runtimeTests } from '@digi/regression/runtime/ready.ts';
import { planSymbol, prepareRuntimePlan } from '@digi/regression/runtime/runtime-plan.ts';
import { createLogNames } from '@digi/dk-session/log-names';

/**
 * Names in the workbench Log pane (docs/ADDRMAP.md "Workbench"), end to end in the emulator over the dk-session path that a device uses too: the
 * load table is read with PEEK of the debug module (`dk.peek`) at the symbol addresses of the image, dkprof.wasm decodes it, and the workbench's own
 * `createLogNames` names the `fini` of a loaded module (`dyn_demo::fini`) and a function of the runtime. Hardware proof is a later, explicit step.
 */
const ID = 'test/dyn-demo';

runtimeTests('the workbench names addresses of the runtime and of a module through PEEK of the debug module', async (runtime) => {
  const { session, dk } = runtime;
  await expectRuntimeReady(session);
  const device = session.options.device;
  const plan = await prepareRuntimePlan(session, liveArtifacts(session, 'log-names'), []);
  const shipMap = path.join(session.runDir, `runtime-${device}.addrmap.json`);
  const artifact = moduleArtifact(device, ID);
  const moduleMap = artifact.replace(/\.dkm$/, '.addrmap.json');
  expect(fs.existsSync(moduleMap), `${moduleMap}: run nx run ${ID}:pack`).toBe(true);
  await installModules(dk, [
    { id: ID, artifact },
    { id: DEBUG_MODULE, artifact: moduleArtifact(device, DEBUG_MODULE) },
  ], { device });

  const wasm = path.join(path.dirname(new URL(import.meta.url).pathname), '../../../../apps/web/public/dkprof.wasm');
  expect(fs.existsSync(wasm), `${wasm}: run nx run dkprof:wasm`).toBe(true);
  const prof = await DkProf.create(new Uint8Array(fs.readFileSync(wasm)));
  // the symbols the load table reads, as the ship map names them, and DKRT_STATE for the module record below
  const { reads } = prof.loadTable({ maps: [JSON.parse(fs.readFileSync(shipMap, 'utf8'))] }) as { reads: { symbol: string }[] };
  const symbols = Object.fromEntries(
    [...new Set(['DKRT_STATE', ...reads.map((r) => r.symbol)])].map((name) => [name, planSymbol(plan, name)]),
  );
  const names = createLogNames({
    prof: async () => prof,
    ship: async () => ({ ship: fs.readFileSync(shipMap, 'utf8'), symbols }),
    moduleMaps: async () => [fs.readFileSync(moduleMap, 'utf8')],
  });
  await names.refresh({ info: () => dk.info(), peek: (addr, len) => dk.peek(addr, len) });
  expect(names.unavailable(), 'the debug module is loaded: PEEK works').toBeUndefined();
  const resolve = names.resolve()!;

  const modules = await dk.modules();
  const slot = modules.find((m) => m.name === ID)!;
  expect(slot, 'the module is loaded').toBeDefined();
  // the module's fini: the loaded record keeps it; the same table the workbench decoded names it
  const fini = Number((await dk.peek(symbols.DKRT_STATE! + 40 + 52 * slot.index + 48, 4)).reduce((a, b) => a * 256 + b, 0));
  expect(fini).toBeGreaterThan(0);
  expect(resolve('cf', fini)).toBe(`${ID}: dyn_demo::fini`);
  expect(resolve('cf', fini + 4)).toBe(`${ID}: dyn_demo::fini+0x4`);
  expect(resolve('cf', planSymbol(plan, 'dkrt_module'))).toBe('dkrt_module');
  await session.expectNoFault('names');
});
