import { readFileSync } from 'node:fs';
import path from 'node:path';
import { Sample, prepareUpload } from '@digi/dk-session/dksample';
import { openSamples } from '@digi/dk-session/samples';
import { button, encoder, repoRoot } from '@digi/test-support';
import { expect } from 'vitest';
import { liveArtifacts, record } from '@digi/regression/runtime/artifacts.ts';
import { installWithDebug } from '@digi/regression/runtime/install.ts';
import { memory, panel } from '@digi/regression/runtime/panel.ts';
import { runtimeTests } from '@digi/regression/runtime/ready.ts';
import { planSymbol, prepareRuntimePlan } from '@digi/regression/runtime/runtime-plan.ts';
import { RATE, selectDynamicMachine, trackApi, wav } from './shot-support.ts';

// DT2 1.17: the glue of digikit/shot with the shared sample picker (docs/design/sample-picker.md): a click on knob D (SMP) opens shot's slot list;
// YES on a slot, a folder and a file loads the file into that pool slot and sets SMP; a held TRIG refuses SMP but the file still loads; NO goes back
// one level; a press with a turn opens no list. The picker logic (order, levels, cursor) is tested once, in modules/core/adapters/ui/host/tests/picker.rs;
// this test is the only one that runs the real hooks (the page click, `smp_pick`, the keys). State comes from the pool API (`sample_pool_status`) and
// SMP (word 0 of `_dv_params`); only which view is on top is read as a screen hash. It cannot run on zero DSP: shot's DSP part is part of its pack, and without a DSP the load fails with E_DSP_XFER (-35).

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
      for (const [name, tones] of [
        ['K1.wav', [440, 660]],
        ['K2.wav', [330, 495]],
      ] as const) {
        const file = prepareUpload(sample, name, wav(3, [...tones]));
        await backend.put(FOLDER, file);
        expect((await backend.verify(FOLDER, file)).ok, `${name} verifies on the card`).toBe(true);
      }

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
      const fileRow = (name: string) => 1 + inFolder.findIndex((e) => e.name === name);
      expect(folderRow).toBeGreaterThan(0);
      expect(fileRow('K1')).toBeGreaterThan(0);
      expect(fileRow('K2')).toBeGreaterThan(0);
      const move = async (key: 'UP' | 'DOWN', n: number) => {
        for (let i = 0; i < n; i++) await ui.tap(key);
      };

      expect((await api.status(0)).state, 'pool slot 0 starts unused').toBe(0);
      expect((await api.status(1)).state, 'pool slot 1 starts unused').toBe(0);

      /** The screen hash: used only to tell the page from the list and the browser (no other state shows which view is on top). */
      const view = async () => (await session.client.framebufferGet({ hash: true, data: false })).sha256 ?? '';
      /** SMP of track 1: `_dv_params` is rewritten only while a voice renders, so TRIG 1 is held for the read. */
      const smp = async () => {
        await session.client.inputButton(button('TRIG 1'), true);
        try {
          await ui.settle(300);
          const peek = await memory(session).dsp(planSymbol(plan, '_dv_params'), 4);
          expect(peek.unknown, 'the SMP word is readable while a voice renders').toBe(0);
          return Buffer.from(peek.hex, 'hex').readUInt32LE(0) >> 8;
        } finally {
          await session.client.inputButton(button('TRIG 1'), false);
        }
      };
      /** Press D, turn `name` once, release D. */
      const pressTurn = async (name: 'ENC D' | 'ENC C') => {
        await session.client.inputButton(button('PUSH D'), true);
        try {
          await ui.settle(100);
          await session.client.inputTurn(encoder(name), 1);
          await ui.settle(200);
        } finally {
          await session.client.inputButton(button('PUSH D'), false);
        }
        await ui.settle(1500);
      };
      /** Click SMP, cursor to `slot`, YES (the browser opens in the root, or in the folder an earlier load left it in), then the file `name` of KICKS
       * with YES. `hold` keeps TRIG 1 down for that last YES. Returns the hash of the browser inside KICKS. */
      const loadSlot = async (slot: number, name: string, inRoot: boolean, hold = false) => {
        await ui.tap('PUSH D', 600);
        await move('UP', 20);
        await move('DOWN', slot);
        await ui.tap('YES', 1500);
        if (inRoot) {
          await move('UP', root.length + 3);
          await move('DOWN', folderRow);
          await ui.tap('YES', 1500);
        }
        const browser = await view();
        await move('UP', inFolder.length + 3);
        await move('DOWN', fileRow(name));
        if (hold) await session.client.inputButton(button('TRIG 1'), true);
        try {
          if (hold) await ui.settle(200);
          await ui.tap('YES', 300);
        } finally {
          if (hold) await session.client.inputButton(button('TRIG 1'), false);
        }
        await ui.settle(1500);
        return browser;
      };
      const page = await view();

      // A click opens the slot list; NO goes back to the page.
      await ui.tap('PUSH D', 600);
      const list = await view();
      expect(list, 'the slot list is not the page').not.toBe(page);
      await ui.tap('NO', 800);
      await ui.settle(1500);
      expect(await view(), 'NO from the list leaves the list').not.toBe(list);

      // Slot 0 gets K1: loaded, SMP 0, the flow closed to the page.
      const browser0 = await loadSlot(0, 'K1', true);
      const ready0 = await api.settle(0);
      expect(ready0.state, 'slot 0 is READY').toBe(3);
      expect(ready0.channels).toBe(2);
      expect(ready0.rate).toBe(RATE);
      expect(await view(), 'the list and the browser are closed: the page is back').not.toBe(browser0);
      expect(await smp(), 'SMP is slot 0').toBe(0);

      // Slot 1 gets K2: SMP follows.
      await loadSlot(1, 'K2', false);
      const ready1 = await api.settle(1);
      expect(ready1.state, 'slot 1 is READY').toBe(3);
      expect(await smp(), 'SMP is slot 1').toBe(1);

      // YES on a filled slot replaces it.
      await loadSlot(0, 'K2', false);
      let replaced = await api.status(0);
      for (let i = 0; i < 100 && !(replaced.state === 3 && replaced.token !== ready0.token); i++) {
        await ui.settle(200);
        replaced = await api.status(0);
      }
      expect(replaced.state, 'slot 0 is READY again').toBe(3);
      expect(replaced.token, 'slot 0 holds a new file').not.toBe(ready0.token);
      expect(await smp(), 'SMP is slot 0').toBe(0);

      // NO goes back one level: KICKS, the root, the slot list, the page. Nothing is picked.
      await ui.tap('PUSH D', 600);
      const list0 = await view();
      await move('UP', 20);
      await ui.tap('YES', 1500); // slot 0: the browser, in KICKS
      const inKicks = await view();
      await ui.tap('NO', 1000);
      const atRoot = await view();
      expect(atRoot, 'NO in KICKS is one folder up, still the browser').not.toBe(inKicks);
      await ui.tap('NO', 1000);
      const listAgain = await view();
      expect(listAgain, 'NO from the browser root is the slot list, not the page').not.toBe(page);
      expect([atRoot, inKicks], 'the slot list is not a browser').not.toContain(listAgain);
      await ui.tap('NO', 800);
      await ui.settle(1500);
      expect(await view(), 'NO again is the page').not.toBe(listAgain);
      expect(await smp(), 'NO changed nothing').toBe(0);

      // A held TRIG refuses the SMP write (E_HELD): the file still loads into slot 1, SMP stays 0.
      await loadSlot(1, 'K1', true, true); // the NO chain above left the browser in the root
      const held = await api.settle(1);
      expect(held.token, 'the file loaded into slot 1 although SMP was refused').not.toBe(ready1.token);
      for (let i = 0; i < 4; i++) await ui.tap('NO', 200); // the list may still be open when the chord did not reach it
      await ui.settle(500);
      expect(await smp(), 'a held TRIG left SMP at 0').toBe(0);

      // A press with a turn is the stock coarse step, and a turn of the unused C also cancels the click: no list opens
      // (`dkrt_page_turn` reaches `turn_input` before the native page takes the event).
      for (const name of ['ENC D', 'ENC C'] as const) {
        await pressTurn(name);
        const after = await view();
        expect(after, `${name}: no slot list opened`).not.toBe(list);
        expect(after, `${name}: no slot list opened (cursor on slot 0)`).not.toBe(list0);
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
  { dsp: 'sharc', devices: ['dt2'], patchSet: 'sample-tracks' },
);
