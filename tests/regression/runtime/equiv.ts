import { createHash } from 'node:crypto';
import path from 'node:path';
import { button, MS, type Device, type Session } from '@digi/test-support';
import { expect, test } from 'vitest';
import { bootInfo } from '@digi/regression/runtime/card-kit.ts';
import { cardRig, RT } from '@digi/regression/runtime/card-rig.ts';
import {
  bounded,
  frames,
  pcmMatches,
  pcmWindow,
  peak,
  pickSine,
  readFloatWav,
  rpc,
} from '@digi/regression/runtime/fx-live.ts';
import { FIRMWARE, FULL_TIER, describeOn } from '@digi/regression/runtime/devices.ts';
import {
  decodeTables,
  diffTables,
  dynType,
  selectType,
  voiceRows,
  type Tables,
} from '@digi/regression/runtime/project-kit.ts';

/**
 * A run-time module set against the patched-in static twin (port of tools/patchkit/pk_digikit_scen.py `run_equiv`).
 * The reference boot (the twin patched in) and the run-time boot (the set loaded from the card) are two daemons; their machine, parameter and page rows are compared.
 * A set without a twin is compared with a boot of no module. A module without a twin has its note pinned by the SHA-256 of its PCM from the onset;
 * a module with a twin (pluck) must play the same note as the twin, sample for sample from the onset.
 */

export interface Equiv {
  name: string;
  device: Device;
  modules: string[];
  /** The patched-in twin; none: the rows are compared with a boot of no module. */
  twin?: string;
  /** The twin leaves no room for the runtime's DSP area: the reference image holds the twin only, with no runtime. */
  refPlain?: boolean;
  /** Full tier only (`DIGIKIT_TIER=full`). */
  full?: boolean;
  /** DT2: the run-time machine to put on track 1 (its type is read from the runtime). DN2: the sound proves the choice. */
  play?: { module: string; machine: string };
  /** DT2: the type number and list position of the patched-in machine on the reference's track 1. */
  staticType?: number;
  /** The list position of the patched-in machine (twin boot, and DT2 run-time hint when `hintRun` is absent). */
  hint?: number;
  /** The list position of the played machine (DN2: the run-time machines follow MIDI). */
  hintRun?: number;
  /** Daemon environment of the run-time boot. */
  envRun?: Record<string, string>;
  trigFrame: number;
  /** Voice rows the set claims. */
  voices: number;
  /** Machines of the run-time set that have no twin: their machine row, parameters and pages are not compared. */
  ignoreRunMachines: string[];
  /** SHA-256 of the PCM from the onset, `PIN_SAMPLES` interleaved float32; none: the note equals the twin's. */
  pcmSha256?: string;
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

export const UNINIT = { SHARC_UNINIT: '5eed1234' }; // the voice state is uninitialised L2 (a run-time module gets no zeroed state): read the model of the device, not zeros
export const DT2_TRIG = 7500;
export const DN2_TRIG = 7000;

/** The note of `file`: TRIG 1 from the DSP block `trigFrame`, the same instruction after the block boundary in every run. */
async function playNote(session: Session, file: string, trigFrame: number): Promise<Float32Array> {
  const current = await frames(session);
  if (current >= trigFrame - 20) throw new Error(`setup took until frame ${current}; raise trigFrame`);
  await rpc(session, 'pcm.tap', { action: 'start' });
  await bounded(() => session.client.runUntil({ frames: trigFrame, timeout_wall_s: 30 }));
  await bounded(() => session.client.step(PRESS_OFFSET, { timeout_wall_s: 30 }));
  await rpc(session, 'pcm.tap', { action: 'capture', path: file });
  await bounded(() => session.client.inputButton(button('TRIG 1'), true));
  // the key stays down for the whole capture: nothing time-dependent is left in the comparison
  await bounded(() => session.client.runUntil({ frames: trigFrame + NOTE_FRAMES, timeout_wall_s: 30 }));
  await rpc(session, 'pcm.tap', { action: 'capture_stop' });
  await bounded(() => session.client.inputButton(button('TRIG 1'), false));
  await bounded(() => session.client.step(50 * MS, { timeout_wall_s: 30 }));
  await bounded(() => session.expectNoFault('note'));
  return readFloatWav(file);
}

/** One `describe` per row: the run-time set and its reference boot in two daemons. */
export function defineEquiv(scenarios: Equiv[]): void {
  for (const sc of scenarios) {
    describeOn(
      sc.device,
      `${sc.device.toUpperCase()} ${FIRMWARE[sc.device]} ${sc.name}`,
      () => {
        const { device } = sc;
        const run = cardRig(device, `${sc.name}-run`, {
          dsp: 'sharc',
          roots: RT,
          hold: true,
          env: sc.envRun,
          symbols: [...SYMBOLS, 'voices.rows', 'dyndsp_info', 'DKRT_ASSIGN'],
          initial: () => ({
            modules: sc.modules,
            selection: sc.modules.map((id) => `modules/${id.split('/').at(-1)}.dkm`),
          }),
        });
        // the reference: the twin patched in, a card with no selection
        const ref = sc.twin
          ? cardRig(device, `${sc.name}-ref`, {
              dsp: 'sharc',
              roots: sc.refPlain ? [sc.twin] : [...RT, sc.twin],
              plain: sc.refPlain,
              symbols: sc.refPlain ? SYMBOLS.filter((x) => !x.startsWith('DKRT_')) : SYMBOLS,
              initial: () => ({}),
              hold: true,
            })
          : undefined;

        /** Machine 1 of the track: DT2 reads the type; DN2 uses the list position (finding 33), the sound proves the type. */
        const pickRun = async (session: Session) => {
          if (!sc.play || device === 'dn2') return pickSine(session, sc.hintRun ?? sc.hint);
          const type = await dynType(session, run.plan, sc.play.module, sc.play.machine);
          await selectType(session, 1, type, sc.hintRun ?? sc.hint ?? 7);
        };

        test('the rows equal the reference; the note matches', async () => {
          let want: Tables;
          let refPcm: Float32Array | undefined;
          if (ref) {
            const refRt = await ref.first();
            want = await decodeTables(refRt.session, ref.plan);
            if (sc.play && !sc.pcmSha256) {
              if (device === 'dt2') await selectType(refRt.session, 1, sc.staticType!, sc.hint ?? 7);
              else await pickSine(refRt.session, sc.hint);
              refPcm = await playNote(refRt.session, path.join(ref.artifacts, 'ref.wav'), sc.trigFrame);
              expect(peak(refPcm), 'the reference note is audible').toBeGreaterThan(0.01);
            }
          }

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

          // the note starts the same instruction after the block boundary of `trigFrame` in every run
          await pickRun(session);
          const runPcm = await playNote(session, path.join(run.artifacts, 'run.wav'), sc.trigFrame);

          if (sc.pcmSha256) {
            // PCM pin: the SHA-256 over PIN_SAMPLES interleaved little-endian float32 samples from the onset (first sample above 1e-6; it lies on a whole stereo frame)
            const { onset, window } = pcmWindow(runPcm, PIN_SAMPLES);
            const sha = createHash('sha256').update(window).digest('hex');
            expect(
              { ok: onset >= 0 && onset % 2 === 0 && window.length / 4 === PIN_SAMPLES, sha },
              `PCM from the onset (${onset}) over ${PIN_SAMPLES} samples has the pinned SHA-256`,
            ).toEqual({ ok: true, sha: sc.pcmSha256 });
          } else if (refPcm) {
            const same = pcmMatches(refPcm, runPcm, 128);
            expect(same.ok, `PCM of the run-time machine equals the patched-in machine's: ${same.detail}`).toBe(true);
          }

          if (!ref) {
            // a set with no twin: the rows are compared with a boot of no module
            const none = await run.load('none', {});
            want = await decodeTables(none.session, run.plan);
          }
          for (const name of sc.ignoreRunMachines) {
            const gone = (got.machines[name] ?? {}) as { knobs?: (string | number)[] };
            delete got.machines[name];
            const mine = got.pages.filter((p) => p.machine === name);
            for (const k of [...(gone.knobs ?? []), ...mine.flatMap((p) => p.knobs)]) delete got.params[String(k)];
            got.pages = got.pages.filter((p) => p.machine !== name);
          }
          const diff = diffTables(got, want!);
          expect(
            diff,
            ref ? "machine, parameter and page rows equal the static twin's" : "rows equal the no-module boot's",
          ).toEqual([]);
          if (ref)
            expect(
              Object.keys(got.machines).length + Object.keys(got.params).length + got.pages.length,
              'the run-time set added rows',
            ).toBeGreaterThan(0);
        }, 1_800_000);
      },
      sc.full === true && !FULL_TIER,
    );
  }
}
