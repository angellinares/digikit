import { readFileSync } from 'node:fs';
import path from 'node:path';
import { Sample, prepareUpload } from '@digi/dk-session/dksample';
import { openSamples } from '@digi/dk-session/samples';
import { button, repoRoot } from '@digi/test-support';
import { expect } from 'vitest';
import { liveArtifacts, record } from '@digi/regression/runtime/artifacts.ts';
import { installWithDebug } from '@digi/regression/runtime/install.ts';
import { memory, panel } from '@digi/regression/runtime/panel.ts';
import { runtimeTests } from '@digi/regression/runtime/ready.ts';
import { planSymbol, prepareRuntimePlan } from '@digi/regression/runtime/runtime-plan.ts';
import { RATE, selectDynamicMachine, trackApi, wav } from './shot-support.ts';

// DT2 1.17: the glue of digikit/shot with the shared sample picker. A click on knob D (SMP) opens shot's slot list; YES on slot 1, on a
// folder and on a file loads that file into pool slot 1. The picker itself (order, levels, cursor, NO and YES) is tested once, in
// modules/core/adapters/ui/host/tests/picker.rs. This test checks the pool through the API (`sample_pool_status`) and SMP (word 0 of `_dv_params`),
// not the screen. It cannot run on zero DSP: shot's DSP part is part of its pack, and without a DSP the load fails with E_DSP_XFER (-35).

const FOLDER = 'KICKS';

runtimeTests(
  'SHOT: a click on SMP opens the slot list and a chosen file is loaded into that pool slot',
  async (runtime) => {
    const { session, dk } = runtime;
    const artifacts = liveArtifacts(session, 'shot-picker');
    const api = trackApi(runtime);
    const ui = panel(session);
    try {
      await installWithDebug(runtime, 'digikit/shot', 'test/track-select', 'test/stock-pool');
      const plan = await prepareRuntimePlan(session, artifacts, ['_dv_params']);
      await selectDynamicMachine(runtime, artifacts);
      await ui.settle(2500);

      const sample = await Sample.create(Uint8Array.from(readFileSync(path.join(repoRoot(), 'apps/web/public/dksample.wasm'))));
      const backend = openSamples('stock-rpc', { session: dk, hashOf: (data) => sample.hash(data) });
      await backend.mkdir(FOLDER);
      const file = prepareUpload(sample, 'K1.wav', wav(3, [440, 660]));
      await backend.put(FOLDER, file);
      expect((await backend.verify(FOLDER, file)).ok, 'K1 verifies on the card').toBe(true);

      // Navigation only: the browser lists folders first, then names without case (row 0 is "..").
      const rank = (e: { name: string; kind: string }) => [e.kind === 'folder' ? 0 : 1, e.name.toLowerCase()] as const;
      const order = async (dir: string) =>
        (await backend.list(dir)).sort((a, b) => {
          const [ka, na] = rank(a);
          const [kb, nb] = rank(b);
          return ka - kb || (na < nb ? -1 : na > nb ? 1 : 0);
        });
      const root = await order('');
      const inFolder = await order(FOLDER);
      const folderRow = 1 + root.findIndex((e) => e.name === FOLDER);
      const fileRow = 1 + inFolder.findIndex((e) => e.name === 'K1');
      expect(folderRow).toBeGreaterThan(0);
      expect(fileRow).toBeGreaterThan(0);
      const move = async (key: 'UP' | 'DOWN', n: number) => {
        for (let i = 0; i < n; i++) await ui.tap(key);
      };

      expect((await api.status(0)).state, 'pool slot 0 starts unused').toBe(0);
      expect((await api.status(1)).state, 'pool slot 1 starts unused').toBe(0);

      await ui.tap('PUSH D', 600); // the slot list
      await move('UP', 20);
      await move('DOWN', 1);
      await ui.tap('YES', 1500); // the browser for slot 1, in the root
      await move('UP', root.length + 3);
      await move('DOWN', folderRow);
      await ui.tap('YES', 1500); // into the folder
      await move('UP', inFolder.length + 3);
      await move('DOWN', fileRow);
      await ui.tap('YES', 300); // load

      const ready = await api.settle(1);
      expect(ready.state, 'slot 1 is READY').toBe(3);
      expect(ready.channels).toBe(2);
      expect(ready.rate).toBe(RATE);
      expect((await api.status(0)).state, 'slot 0 is still unused').toBe(0);

      // SMP follows the pick. `_dv_params` is rewritten only while a voice renders, so TRIG 1 is held for the read.
      await ui.settle(2500);
      await session.client.inputButton(button('TRIG 1'), true);
      try {
        await ui.settle(300);
        const peek = await memory(session).dsp(planSymbol(plan, '_dv_params'), 4);
        expect(peek.unknown, 'the SMP word is readable while a voice renders').toBe(0);
        expect(Buffer.from(peek.hex, 'hex').readUInt32LE(0) >> 8, 'SMP is slot 1').toBe(1);
      } finally {
        await session.client.inputButton(button('TRIG 1'), false);
      }

      for (const m of await dk.modules()) expect(m.faults, `${m.name} faults`).toBe(0);
      await session.expectNoFault('SMP click flow');
    } finally {
      try {
        record(artifacts, 'runtime-log', await dk.readLog());
      } catch {
        // The log is a diagnostic only.
      }
    }
  },
  { dsp: 'sharc', devices: ['dt2'], patchSet: 'sample-tracks', timeout: 600_000 },
);
