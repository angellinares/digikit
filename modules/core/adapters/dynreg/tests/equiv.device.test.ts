import { createHash } from 'node:crypto';
import path from 'node:path';
import { button, MS, type Device } from '@digi/test-support';
import { describe, expect, test } from 'vitest';
import { bootInfo } from '@digi/regression/runtime/card-kit.ts';
import { cardRig, RT } from '@digi/regression/runtime/card-rig.ts';
import { bounded, frames, pcmWindow, pickSine, readFloatWav, rpc } from '@digi/regression/runtime/fx-live.ts';
import { FIRMWARE } from '@digi/regression/runtime/devices.ts';
import { decodeTables, diffTables, voiceRows } from '@digi/regression/runtime/project-kit.ts';

/**
 * A run-time module set against the patched-in static twin (port of tools/patchkit/pk_digikit_scen.py `run_equiv`, scenario equiv-dn2-set).
 * The reference boot (the twin patched in) and the run-time boot (the set loaded from the card) are two daemons; their machine, parameter and page rows are compared.
 * A module without a twin has its note pinned by the SHA-256 of its PCM from the onset.
 */

interface Equiv {
  name: string;
  device: Device;
  modules: string[];
  twin: string;
  /** The list position of the played machine (DN2: the run-time machines follow MIDI). */
  hintRun: number;
  trigFrame: number;
  /** Voice rows the set claims. */
  voices: number;
  /** Machines of the run-time set that have no twin: their machine row, parameters and pages are not compared. */
  ignoreRunMachines: string[];
  /** SHA-256 of the PCM from the onset, `PIN_SAMPLES` interleaved float32. */
  pcmSha256: string;
}

const PIN_SAMPLES = 24000; // interleaved samples a PCM pin covers from the onset (0.25 s of stereo)
const PRESS_OFFSET = 44_000; // ColdFire instructions after the block boundary (a block is 88 000)
const NOTE_FRAMES = 1500; // 1 s of audio, captured from the trig on
const FREE_TYPE = 0xffff_ffff;
const SYMBOLS = [
  'DKRT_BOOT',
  'DKRT_ASSIGN',
  'machines.rows',
  'machines.count',
  'machines.first',
  'machines.row_size',
  'pf_table',
  'params.first',
  'params.count',
];

const SCENARIOS: Equiv[] = [
  {
    name: 'equiv-dn2-set',
    device: 'dn2',
    modules: ['test/dyn-dn2', 'test/sine'],
    twin: 'test/twin-dn2',
    hintRun: 6,
    trigFrame: 7000,
    voices: 2,
    ignoreRunMachines: ['Sine'],
    // onset 400, 24000 samples (DN2 1.12, note 60, 261.65 Hz); approved by Em 2026-11-01 (exact RECIPS, finding 102)
    pcmSha256: '330e9725263aa2aab1da1c4f9b562b5b0d64c9896212106c4eddc3c4547db1d3',
  },
];

for (const sc of SCENARIOS) {
  describe(`${sc.device.toUpperCase()} ${FIRMWARE[sc.device]} ${sc.name}`, () => {
    const { device } = sc;
    const run = cardRig(device, `${sc.name}-run`, { dsp: 'sharc',
      roots: RT,
      hold: true,
      symbols: [...SYMBOLS, 'voices.rows', 'dyndsp_info'],
      initial: () => ({
        modules: sc.modules,
        selection: sc.modules.map((id) => `modules/${id.split('/').at(-1)}.dkm`),
      }),
    });
    // the reference: the twin patched in, a card with no selection
    const ref = cardRig(device, `${sc.name}-ref`, { dsp: 'sharc',
      roots: [...RT, sc.twin],
      symbols: SYMBOLS,
      initial: () => ({}),
      hold: true,
    });

    test('the rows equal the static twin; the sine note has the pinned SHA-256', async () => {
      const refRt = await ref.first();
      const want = await decodeTables(refRt.session, ref.plan);

      const rt = await run.first();
      const { session } = rt;
      const info = await bootInfo(session, run.plan);
      const n = sc.modules.length;
      expect(info.count, 'the selection loaded at boot').toBe(n);
      expect(info.results, 'every module result is 0').toEqual(Array(n).fill(0));
      expect([info.loaded, info.safe]).toEqual([n, 0]);
      const got = await decodeTables(session, run.plan);
      const used = (await voiceRows(session, run.plan)).filter(([type]) => type !== FREE_TYPE);
      expect(used.length, `voice rows claimed: ${JSON.stringify(used)}`).toBe(sc.voices);

      // the DN2 machine list position (finding 33); the sound proves the type. The note starts the same instruction after the block boundary of `trigFrame` in every run.
      await pickSine(session, sc.hintRun);
      const current = await frames(session);
      if (current >= sc.trigFrame - 20) throw new Error(`setup took until frame ${current}; raise trigFrame`);
      const file = path.join(run.artifacts, 'run.wav');
      await rpc(session, 'pcm.tap', { action: 'start' });
      await bounded(() => session.client.runUntil({ frames: sc.trigFrame, timeout_wall_s: 30 }));
      await bounded(() => session.client.step(PRESS_OFFSET, { timeout_wall_s: 30 }));
      await rpc(session, 'pcm.tap', { action: 'capture', path: file });
      await bounded(() => session.client.inputButton(button('TRIG 1'), true));
      // the key stays down for the whole capture: nothing time-dependent is left in the comparison
      await bounded(() => session.client.runUntil({ frames: sc.trigFrame + NOTE_FRAMES, timeout_wall_s: 30 }));
      await rpc(session, 'pcm.tap', { action: 'capture_stop' });
      await bounded(() => session.client.inputButton(button('TRIG 1'), false));
      await bounded(() => session.client.step(50 * MS, { timeout_wall_s: 30 }));
      await bounded(() => session.expectNoFault('note'));

      // PCM pin: the SHA-256 over PIN_SAMPLES interleaved little-endian float32 samples from the onset (first sample above 1e-6; it lies on a whole stereo frame)
      const { onset, window } = pcmWindow(readFloatWav(file), PIN_SAMPLES);
      const sha = createHash('sha256').update(window).digest('hex');
      expect(
        { ok: onset >= 0 && onset % 2 === 0 && window.length / 4 === PIN_SAMPLES, sha },
        `PCM from the onset (${onset}) over ${PIN_SAMPLES} samples has the pinned SHA-256`,
      ).toEqual({ ok: true, sha: sc.pcmSha256 });

      for (const name of sc.ignoreRunMachines) {
        const gone = (got.machines[name] ?? {}) as { knobs?: (string | number)[] };
        delete got.machines[name];
        const mine = got.pages.filter((p) => p.machine === name);
        for (const k of [...(gone.knobs ?? []), ...mine.flatMap((p) => p.knobs)]) delete got.params[String(k)];
        got.pages = got.pages.filter((p) => p.machine !== name);
      }
      const diff = diffTables(got, want);
      expect(diff, "machine, parameter and page rows equal the static twin's").toEqual([]);
      expect(
        Object.keys(got.machines).length + Object.keys(got.params).length + got.pages.length,
        'the run-time set added rows',
      ).toBeGreaterThan(0);
    }, 1_800_000);
  });
}
