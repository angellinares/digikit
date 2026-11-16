import { createHash } from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { button, MS, stepWithProgress, type Device, type Session } from '@digi/test-support';
import { expect, test } from 'vitest';
import { bootInfo } from '@digi/regression/runtime/card-kit.ts';
import { readLoadTable, resolveAddresses } from '@digi/regression/runtime/addrmap.ts';
import { moduleArtifact } from '@digi/regression/runtime/install.ts';
import { moduleResultsNote } from '@digi/regression/runtime/runtime-menu.ts';
import { cardRig, RT } from '@digi/regression/runtime/card-rig.ts';
import {
  bounded,
  frames,
  pcmWindow,
  peak,
  pickSine,
  readFloatWav,
  rpc,
  runToFrame,
  type Rt,
} from '@digi/regression/runtime/fx-live.ts';
import { FIRMWARE, FULL_TIER, describeOn } from '@digi/regression/runtime/devices.ts';
import {
  decodeTables,
  diffTables,
  dynType,
  FREE_TYPE,
  selectType,
  voiceRows,
} from '@digi/regression/runtime/project-kit.ts';

/**
 * A run-time module set against a boot of no module (port of tools/patchkit/pk_digikit_scen.py `run_equiv`).
 * The run-time boot (the set loaded from the card) has the machine, parameter and page rows of a boot of no module, apart from the machines named in `ignoreRunMachines`.
 * A set with a sound has its note pinned by the SHA-256 of its PCM from the onset.
 */

export interface Equiv {
  name: string;
  device: Device;
  modules: string[];
  /** Full tier only (`DIGIKIT_TIER=full`). */
  full?: boolean;
  /** DT2: the run-time machine to put on track 1 (its type is read from the runtime). DN2: the sound proves the choice. */
  play?: { module: string; machine: string };
  /** The list position of the played machine (DN2: the run-time machines follow MIDI; DT2: where the type is chosen). */
  hint?: number;
  /** Daemon environment of the run-time boot. */
  envRun?: Record<string, string>;
  trigFrame: number;
  /** Voice rows the set claims. */
  voices: number;
  /** Machines of the run-time set: their machine row, parameters and pages are not compared. */
  ignoreRunMachines: string[];
  /** The address map names a DSP function of a loaded module: the load table is read from the running emulator and the symbol's address in the module's own map resolves to it (docs/ADDRMAP.md). */
  dspName?: { module: string; symbol: string };
  /** SHA-256 of the PCM from the onset, `PIN_SAMPLES` interleaved float32; none: the note is only audible. */
  pcmSha256?: string;
}

/** A pin whose SHA-256 is not approved yet: the case fails and names the measured hash, which gate BC produces and Em approves before it replaces the placeholder. */
export const PENDING_PCM = 'pending-approval';
const PIN_SAMPLES = 24000; // interleaved samples a PCM pin covers from the onset (0.25 s of stereo)
const PRESS_OFFSET = 44_000; // ColdFire instructions after the block boundary (a block is 88 000)
const NOTE_FRAMES = 1500; // 1 s of audio, captured from the trig on
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
  await runToFrame(session, trigFrame);
  await stepWithProgress(session.client, PRESS_OFFSET);
  await rpc(session, 'pcm.tap', { action: 'capture', path: file });
  await bounded(() => session.client.inputButton(button('TRIG 1'), true));
  // the key stays down for the whole capture: nothing time-dependent is left in the comparison
  await runToFrame(session, trigFrame + NOTE_FRAMES);
  await rpc(session, 'pcm.tap', { action: 'capture_stop' });
  await bounded(() => session.client.inputButton(button('TRIG 1'), false));
  await stepWithProgress(session.client, 50 * MS);
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
        /** Machine 1 of the track: DT2 reads the type; DN2 uses the list position (finding 33), the sound proves the type. */
        const pickRun = async (rt: Rt) => {
          if (!sc.play || device === 'dn2') return pickSine(rt, sc.hint);
          const type = await dynType(rt.session, run.plan, sc.play.module, sc.play.machine);
          await selectType(rt, 1, type, sc.hint ?? 7);
        };

        test('the rows equal the reference; the note matches', async () => {
          const rt = await run.first();
          const { session } = rt;
          const info = await bootInfo(session, run.plan);
          const n = sc.modules.length;
          expect(info.count, 'the selection loaded at boot').toBe(n);
          expect(
            info.results,
            `every module result is 0${await moduleResultsNote(session, run.plan, info.results)}`,
          ).toEqual(Array(n).fill(0));
          expect([info.loaded, info.safe]).toEqual([n, 0]);
          const got = await decodeTables(session, run.plan);
          const used = (await voiceRows(session, run.plan)).filter(([type]) => type !== FREE_TYPE);
          expect(used.length, `voice rows claimed: ${JSON.stringify(used)}`).toBe(sc.voices);

          if (sc.dspName) {
            const { module, symbol } = sc.dspName;
            const shipMap = path.join(run.artifacts, `runtime-${device}.addrmap.json`);
            const moduleMap = moduleArtifact(device, module).replace(/\.dkm$/, '.addrmap.json');
            const load = (await readLoadTable(session, run.plan, shipMap)).find((l) => l.id === module);
            expect(load?.dspBase, `${module} has a DSP image start`).toBeGreaterThan(0);
            const entry = (
              JSON.parse(fs.readFileSync(moduleMap, 'utf8')) as {
                entries: { cpu: string; addr: number; name: string }[];
              }
            ).entries.find((e) => e.cpu === 'sharc' && e.name === symbol);
            expect(entry, `${symbol} in the module map`).toBeDefined();
            const [hit] = await resolveAddresses(
              [shipMap, moduleMap],
              [load!],
              [load!.dspBase! + entry!.addr],
              'sharc',
            );
            expect(hit, `the DSP address of ${symbol}`).toMatchObject({
              name: symbol,
              layer: 'our',
              offset: 0,
              module,
            });
          }

          // the note starts the same instruction after the block boundary of `trigFrame` in every run
          await pickRun(rt);
          const runPcm = await playNote(session, path.join(run.artifacts, 'run.wav'), sc.trigFrame);

          if (sc.pcmSha256) {
            // PCM pin: the SHA-256 over PIN_SAMPLES interleaved little-endian float32 samples from the onset (first sample above 1e-6; it lies on a whole stereo frame)
            const { onset, window } = pcmWindow(runPcm, PIN_SAMPLES);
            const sha = createHash('sha256').update(window).digest('hex');
            if (sc.pcmSha256 === PENDING_PCM) {
              throw new Error(
                `${sc.name}: the PCM pin is pending approval; measured onset ${onset}, SHA-256 ${sha} (gate BC; Em approves it, then it replaces PENDING_PCM)`,
              );
            }
            expect(
              { ok: onset >= 0 && onset % 2 === 0 && window.length / 4 === PIN_SAMPLES, sha },
              `PCM from the onset (${onset}) over ${PIN_SAMPLES} samples has the pinned SHA-256`,
            ).toEqual({ ok: true, sha: sc.pcmSha256 });
          } else if (sc.play) {
            // no pin: the note is only audible
            expect(peak(runPcm), 'the run-time note is audible').toBeGreaterThan(0.01);
          }

          // the rows are compared with a boot of no module
          const none = await run.load('none', {});
          const want = await decodeTables(none.session, run.plan);
          for (const name of sc.ignoreRunMachines) {
            const gone = (got.machines[name] ?? {}) as { knobs?: (string | number)[] };
            delete got.machines[name];
            const mine = got.pages.filter((p) => p.machine === name);
            for (const k of [...(gone.knobs ?? []), ...mine.flatMap((p) => p.knobs)]) delete got.params[String(k)];
            got.pages = got.pages.filter((p) => p.machine !== name);
          }
          expect(diffTables(got, want), "rows equal the no-module boot's").toEqual([]);
        });
      },
      sc.full === true && !FULL_TIER,
    );
  }
}
