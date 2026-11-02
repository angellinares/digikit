import { execFile } from 'node:child_process';
import { mkdirSync, readFileSync } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { promisify } from 'node:util';
import { Sample, prepareUpload } from '@digi/dk-session/dksample';
import { openSamples } from '@digi/dk-session/samples';
import { button, encoder, repoRoot } from '@digi/test-support';
import { expect } from 'vitest';
import { installWithDebug, liveArtifacts, memory, panel, record, runtimeTests, settleUi } from '../../../../tests/regression/runtime/live.ts';
import { planSymbol, prepareRuntimePlan } from '../../../../tests/regression/runtime/runtime-plan.ts';
import {
  RATE,
  captureNote,
  expectSplitPair,
  onset,
  selectDynamicMachine,
  trackApi,
  until,
  wav,
} from './shot-support.ts';

// DT2 1.17: the SMP click flow of digikit/shot (docs/design/ui-rules.md, docs/design/machine-page-click.md, docs/design/sample-picker.md):
// a click on knob B (press and release without a turn) opens the slot list 00..15; YES on a slot (empty or filled) opens the file browser for
// that slot; YES on a file loads it into that slot, sets SMP to the slot and returns to the page; NO goes back one level; a held TRIG keeps SMP
// unchanged; a press with a turn is the stock coarse step and opens no list.
//
// The state is read from the runtime, not from the framebuffer: SMP is word 0 of `_dv_params` (raw = entry << 8, rewritten only while a voice
// renders, so TRIG 1 is held for each read), the pool entries come from `sample_pool_status` through test/track-select, and the played tones
// come from the PCM tap. The framebuffer hash only tells "page" from "list"; every screen is also saved as a PNG for a look.

const execFileAsync = promisify(execFile);
const FOLDER = 'KICKS';
const K1 = [440, 660] as const;
const K2 = [330, 495] as const;

/** The order the picker shows a folder in: folders first, then names, case-insensitive. */
const pickerOrder = (entries: { name: string; kind: 'file' | 'folder' }[]) =>
  [...entries].sort((a, b) => {
    const [x, y] = [a.name.toLowerCase(), b.name.toLowerCase()];
    return Number(b.kind === 'folder') - Number(a.kind === 'folder') || (x < y ? -1 : x > y ? 1 : 0);
  });

runtimeTests(
  'SHOT: a click on SMP opens the slot list; YES on a slot and a file loads it into that slot and returns to the page',
  async (runtime) => {
    const { session, dk } = runtime;
    const artifacts = liveArtifacts(session, 'shot-picker');
    const shotDir = path.join(process.env.SHOT_PICKER_DIR ?? os.tmpdir(), 'shot-picker-screens');
    mkdirSync(shotDir, { recursive: true });
    const api = trackApi(runtime);
    const ui = panel(session);
    const log: Record<string, unknown> = {};
    const hashes: Record<string, string> = {};
    /** Save the screen as a PNG and return its hash. */
    const screen = async (name: string): Promise<string> => {
      await execFileAsync(
        'uv',
        ['run', 'python', '-m', 'tools.control.digi_client', '--run-dir', session.runDir, 'frame', path.join(shotDir, `${name}.png`)],
        { cwd: repoRoot(), timeout: 20_000 },
      );
      const fb = await session.client.framebufferGet({ hash: true, data: false });
      hashes[name] = fb.sha256 ?? '';
      return hashes[name]!;
    };
    const down = async (n: number) => {
      for (let i = 0; i < n; i++) await ui.tap('DOWN');
    };
    const up = async (n: number) => {
      for (let i = 0; i < n; i++) await ui.tap('UP');
    };
    const click = async () => ui.tap('PUSH B', 600);
    try {
      await settleUi(session);
      await installWithDebug(runtime, 'digikit/shot', 'test/track-select', 'test/stock-pool');
      await settleUi(session);
      const plan = await prepareRuntimePlan(session, artifacts, ['_dv_params']);
      await selectDynamicMachine(runtime, artifacts);
      await ui.settle(2500);
      const pageHash = await screen('00-page');

      /** SMP raw word of track 1: TRIG 1 held until the voice renders and rewrites `_dv_params`. Releases the key before it returns. */
      const mem = memory(session);
      const smpWord = async (): Promise<number> => {
        await session.client.inputButton(button('TRIG 1'), true);
        try {
          await ui.settle(300);
          const peek = await mem.dsp(planSymbol(plan, '_dv_params'), 4);
          expect(peek.unknown, 'the SMP word is readable while a voice renders').toBe(0);
          return Buffer.from(peek.hex, 'hex').readUInt32LE(0);
        } finally {
          await session.client.inputButton(button('TRIG 1'), false);
        }
      };
      const smp = async () => (await smpWord()) >> 8;
            /** Back on the SHOT page from wherever a flow stopped: NO closes the open levels, SRC shows the page of the selected track. */
      const toPage = async () => {
        for (let i = 0; i < 4; i++) await ui.tap('NO', 200);
        await ui.tap('SRC', 500);
      };
      /** A note on TRIG 1: the tones of the pair, split L / R. */
      const noteIs = async (name: string, pair: readonly [number, number]) => {
        const note = await captureNote(runtime, path.join(artifacts, `${name}.wav`), async (clock) => {
          await until(clock, (await clock()) + 1.5 * RATE);
        });
        const o = onset(note);
        expect(o, `${name}: the note is audible`).toBeGreaterThanOrEqual(0);
        return expectSplitPair(name, note, o + 4800, o + RATE, pair[0], pair[1]);
      };

      // ---- fixtures: KICKS/K1 (L 440, R 660) and KICKS/K2 (L 330, R 495), stereo 48 kHz, on +Drive -----------------------------------
      const sample = await Sample.create(Uint8Array.from(readFileSync(path.join(repoRoot(), 'apps/web/public/dksample.wasm'))));
      const backend = openSamples('stock-rpc', { session: dk, hashOf: (data) => sample.hash(data) });
      await backend.mkdir(FOLDER);
      for (const [name, tones] of [['K1.wav', K1], ['K2.wav', K2]] as const) {
        const file = prepareUpload(sample, name, wav(3, [...tones]));
        expect(file.kind).toBe('converted');
        await backend.put(FOLDER, file);
        expect((await backend.verify(FOLDER, file)).ok, `${name} verifies on the card`).toBe(true);
      }
      const root = pickerOrder(await backend.list(''));
      const inFolder = pickerOrder(await backend.list(FOLDER));
      log.listing = { root, inFolder };
      const folderRow = 1 + root.findIndex((e) => e.name === FOLDER); // row 0 is ".."
      const fileRow = (name: string) => 1 + inFolder.findIndex((e) => e.name === name);
      expect(folderRow).toBeGreaterThan(0);
      expect(fileRow('K1')).toBeGreaterThan(0);
      expect(fileRow('K2')).toBeGreaterThan(0);
      expect((await api.status(0)).state, 'pool entry 0 starts unused').toBe(0);
      expect((await api.status(1)).state, 'pool entry 1 starts unused').toBe(0);

      /** Click SMP, move the cursor to `slot`, YES (the browser opens for it, in the root or in the folder an earlier load left it in), then the
       * file `name` of KICKS with YES: the file loads into the slot and the flow closes. `hold` keeps TRIG 1 down for that last YES. */
      const loadSlot = async (slot: number, name: string, shotName: string, inRoot: boolean, hold = false) => {
        await click();
        await screen(`${shotName}-list`);
        await up(20);
        await down(slot);
        await ui.tap('YES', 1500);
        await screen(`${shotName}-browser`);
        if (inRoot) {
          await up(root.length + 3);
          await down(folderRow);
          await ui.tap('YES', 1500);
          await screen(`${shotName}-folder`);
        }
        await up(inFolder.length + 3);
        await down(fileRow(name));
        if (hold) await session.client.inputButton(button('TRIG 1'), true);
        try {
          if (hold) await ui.settle(200);
          await ui.tap('YES', 300);
          if (hold) await screen(`${shotName}-held-yes`);
        } finally {
          if (hold) await session.client.inputButton(button('TRIG 1'), false);
        }
      };

      // ---- 1. a click opens the slot list; NO goes back to the page -------------------------------------------------------------------------
      // The pool is empty, so no voice renders and SMP cannot be read yet; the page screen is the witness.
      await click();
      const listHash = await screen('01-list');
      expect(listHash, 'the pool list is not the page').not.toBe(pageHash);
      await ui.tap('NO', 800);
      await ui.settle(1500);
      // The page shows the touched value ("0" in place of the label SMP), so its hash is not the idle page's: the list is gone when the screen is not the list.
      expect(await screen('02-no-from-list'), 'NO from the list leaves the list').not.toBe(listHash);

      // ---- 2. slot 0 gets K1: YES on the empty slot, YES on the file: loaded into slot 0, SMP 0, back on the page ----------------------
      await loadSlot(0, 'K1', '03-slot0-k1', true);
      const readyK1 = await api.settle(0);
      log.readyK1 = readyK1;
      expect(readyK1.state, 'slot 0 is READY').toBe(3);
      expect(readyK1.channels).toBe(2);
      expect(readyK1.rate).toBe(RATE);
      await ui.settle(2500);
      const afterK1 = await screen('04-after-slot0-k1');
      expect(afterK1, 'the list and the browser are closed: the page is back').not.toBe(hashes['03-slot0-k1-folder']);
      log.smpAfterK1 = await smp();
      expect(log.smpAfterK1, 'SMP is slot 0').toBe(0);
      expect(await smp(), 'the flow is closed: still slot 0').toBe(0);
      log.noteK1 = await noteIs('k1', K1);
      await session.expectNoFault('slot 0 K1');

      // ---- 3. slot 1 gets K2: SMP 1 --------------------------------------------------------------------------------------------------
      // The list opens with the cursor on slot 0 (the current SMP); the browser opens in the folder it was left in.
      await loadSlot(1, 'K2', '05-slot1-k2', false);
      const readyK2 = await api.settle(1);
      log.readyK2 = readyK2;
      expect(readyK2.state, 'slot 1 is READY').toBe(3);
      await ui.settle(2500);
      await screen('06-after-slot1-k2');
      expect(await smp(), 'SMP is slot 1').toBe(1);
      log.noteK2 = await noteIs('k2', K2);

      // ---- 4. YES on the filled slot 0 replaces it: K2 now plays from slot 0 ----------------------------------------------------------
      await loadSlot(0, 'K2', '07-replace-slot0', false);
      let replaced = await api.status(0);
      for (let i = 0; i < 100 && !(replaced.state === 3 && replaced.token !== readyK1.token); i++) {
        await ui.settle(200);
        replaced = await api.status(0);
      }
      log.replacedSlot0 = replaced;
      expect(replaced.state, 'slot 0 is READY again').toBe(3);
      expect(replaced.token, 'slot 0 holds a new file').not.toBe(readyK1.token);
      await ui.settle(2500);
      await screen('08-after-replace');
      expect(await smp(), 'SMP is slot 0').toBe(0);
      log.noteReplaced = await noteIs('slot0-is-k2', K2);

      // ---- 5. NO inside the flow goes back one level ------------------------------------------------------------------------------------
      await click();
      const list0 = await screen('09-list');
      await up(20);
      await ui.tap('YES', 1500); // slot 0: the browser, in KICKS
      const browserHash = await screen('10-browser');
      // The browser is in KICKS: NO goes up to the root, NO again leaves the browser for the slot list, NO again the page.
      await ui.tap('NO', 1000);
      const rootHash = await screen('10b-browser-root');
      expect(rootHash, 'NO in KICKS is one folder up, still the browser').not.toBe(browserHash);
      await ui.tap('NO', 1000);
      const listAgain = await screen('11-list-after-no');
      expect(listAgain, 'NO from the browser root is the slot list, not the page').not.toBe(pageHash);
      expect(listAgain).not.toBe(rootHash);
      expect(listAgain).not.toBe(browserHash);
      log.sameListAfterNo = listAgain === list0; // a record only: the cursor returns to the slot that was chosen
      await ui.tap('NO', 800); // the page
      await ui.settle(1500);
      await screen('12-page-after-no');
      expect(await smp(), 'NO changed nothing').toBe(0);

      // ---- 6. a held TRIG refuses the SMP write (E_HELD): the file loads into slot 1, SMP stays 0 -------------------------------------
      await loadSlot(1, 'K1', '13-held', true, true) // step 5 left the browser in the root;
      await ui.settle(800);
      await screen('14-held-after');
      await toPage(); // the list may still be open when the chord did not reach it
      expect(await smp(), 'a held TRIG left SMP at 0').toBe(0);

      // ---- 7. a press with a turn is the stock coarse step: no list ---------------------------------------------------------------------------
      const before = await smp();
      await session.client.inputButton(button('PUSH B'), true);
      try {
        await ui.settle(100);
        await session.client.inputTurn(encoder('ENC B'), 1);
        await ui.settle(200);
      } finally {
        await session.client.inputButton(button('PUSH B'), false);
      }
      await ui.settle(1500);
      const turned = await screen('15-press-turn');
      expect(turned, 'no slot list opened').not.toBe(listHash);
      expect(turned).not.toBe(list0);
      const after = await smp();
      log.pressTurn = { before, after };
      // No check on the value: one slow detent moves a parameter by about 1/256 of a unit (stock acceleration, finding 02), and
      // `_dv_params` is only rewritten while a voice plays. The point of this step is that a press with a turn opens no list.

      const modules = await dk.modules();
      for (const m of modules) expect(m.faults, `${m.name} faults`).toBe(0);
      await session.expectNoFault('SMP click flow');
    } finally {
      record(artifacts, 'measurements', { log, hashes });
      console.info('shot-picker measurements', JSON.stringify({ log, hashes, shotDir }));
      try {
        record(artifacts, 'runtime-log', await dk.readLog());
      } catch {
        // The log is a diagnostic only.
      }
    }
  },
  { devices: ['dt2'], patchSet: 'sample-tracks', timeout: 1_500_000 },
);
