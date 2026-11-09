import { createHash } from 'node:crypto';
import path from 'node:path';
import { button, MS, type Device, type Session } from '@digi/test-support';
import { describe, expect, test } from 'vitest';
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
import { FIRMWARE, FULL_TIER } from '@digi/regression/runtime/devices.ts';
import { decodeTables, diffTables, dynType, selectType, voiceRows, type Tables } from '@digi/regression/runtime/project-kit.ts';

/**
 * A run-time module set against the patched-in static twin (port of tools/patchkit/pk_digikit_scen.py `run_equiv`).
 * The reference boot (the twin patched in) and the run-time boot (the set loaded from the card) are two daemons; their machine, parameter and page rows are compared.
 * A set without a twin is compared with a boot of no module. A module without a twin has its note pinned by the SHA-256 of its PCM from the onset;
 * a module with a twin (pluck) must play the same note as the twin, sample for sample from the onset.
 */

interface Equiv {
  name: string;
  device: Device;
  modules: string[];
  /** The patched-in twin; none: the rows are compared with a boot of no module. */
  twin?: string;
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

const UNINIT = { SHARC_UNINIT: '5eed1234' }; // the voice state is uninitialised L2 (a run-time module gets no zeroed state): read the model of the device, not zeros
const DT2_TRIG = 7500;
const DN2_TRIG = 7000;

// Not ported: equiv-dyn-pluck on DN2 (Python `equiv-dyn-pluck` keeps only that device). Its patched-in twin leaves no room for the runtime's
// 320 KiB DSP area, so the reference image has no runtime, which the card rig cannot boot (it plans DKRT_BOOT).
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
  {
    // the note starts shortly after the panel setup ends (frame 3330 in both rigs)
    name: 'equiv-dyn-pluck',
    device: 'dt2',
    modules: ['digikit/pluck'],
    twin: 'test/patched-pluck',
    play: { module: 'digikit/pluck', machine: 'pluck' },
    staticType: 9,
    hint: 7,
    trigFrame: 3500,
    voices: 1,
    ignoreRunMachines: [],
  },
  {
    // Full tier: the same twin comparison with the note later in the run
    name: 'equiv-mixed',
    device: 'dt2',
    full: true,
    modules: ['digikit/pluck'],
    twin: 'test/patched-pluck',
    play: { module: 'digikit/pluck', machine: 'pluck' },
    staticType: 9,
    hint: 7,
    trigFrame: 9500,
    voices: 1,
    ignoreRunMachines: [],
  },
  {
    name: 'equiv-dyn-analog',
    device: 'dt2',
    modules: ['fadedlimes/analog'],
    play: { module: 'fadedlimes/analog', machine: 'analog' },
    hint: 7,
    trigFrame: DT2_TRIG,
    voices: 1,
    ignoreRunMachines: ['Analog'],
    envRun: UNINIT,
    // onset 256, 24000 samples (DT2 1.17, note 60); approved by Em 2026-11-01 (exact RECIPS, finding 102)
    pcmSha256: '427c8f33e23779303e4a2161e77c168f9b5e4833522fba23d901f3442ffeeb8a',
  },
  {
    name: 'equiv-dyn-analog',
    device: 'dn2',
    modules: ['fadedlimes/analog'],
    play: { module: 'fadedlimes/analog', machine: 'analog' },
    hint: 4,
    hintRun: 5,
    trigFrame: DN2_TRIG,
    voices: 1,
    ignoreRunMachines: ['Analog'],
    envRun: UNINIT,
    // onset 328, 24000 samples (DN2 1.12, note 60); approved by Em 2026-11-01 (exact RECIPS, finding 102)
    pcmSha256: '44db3540cf67de6e20ef5cda8e15f5a45bef5bbdc6c86213a7f6ea1dbf8f651e',
  },
  {
    name: 'equiv-dyn-bd',
    device: 'dt2',
    modules: ['fadedlimes/bd'],
    play: { module: 'fadedlimes/bd', machine: 'bd' },
    hint: 7,
    trigFrame: DT2_TRIG,
    voices: 1,
    ignoreRunMachines: ['Bass Drum'],
    // onset 194, 24000 samples (DT2 1.17, note 60); approved by Em 2026-11-01 (exact RECIPS, finding 102)
    pcmSha256: '8de331fb3fec4648ef530b0f56e1fd8cf2634c094e7dd4631b472c2942f5dceb',
  },
  {
    name: 'equiv-dyn-bd',
    device: 'dn2',
    modules: ['fadedlimes/bd'],
    play: { module: 'fadedlimes/bd', machine: 'bd' },
    hint: 4,
    hintRun: 5,
    trigFrame: DN2_TRIG,
    voices: 1,
    ignoreRunMachines: ['Bass Drum'],
    // onset 270, 24000 samples (DN2 1.12, note 60); approved by Em 2026-11-01 (exact RECIPS, finding 102)
    pcmSha256: '5838f6e275cc21b45d6c97b43e1b48e1e6876517b18b07c527dae611b1325622',
  },
  {
    name: 'equiv-dyn-pd',
    device: 'dt2',
    modules: ['fadedlimes/pd'],
    play: { module: 'fadedlimes/pd', machine: 'pd' },
    hint: 7,
    trigFrame: DT2_TRIG,
    voices: 1,
    ignoreRunMachines: ['Phase Dist'],
    envRun: UNINIT,
    // onset 258, 24000 samples (DT2 1.17, note 60); approved by Em 2026-11-01 (exact RECIPS, finding 102)
    pcmSha256: 'eed2bf269be58b6bac0ba329007f1221c2f0920fd46eac60aa43d300cedb97e0',
  },
  {
    name: 'equiv-dyn-pd',
    device: 'dn2',
    modules: ['fadedlimes/pd'],
    play: { module: 'fadedlimes/pd', machine: 'pd' },
    hint: 4,
    hintRun: 5,
    trigFrame: DN2_TRIG,
    voices: 1,
    ignoreRunMachines: ['Phase Dist'],
    envRun: UNINIT,
    // onset 272, 24000 samples (DN2 1.12, note 60); approved by Em 2026-11-01 (exact RECIPS, finding 102)
    pcmSha256: 'a8143de5a6e1235c2fd1cfb2389a8cc05dd3824d0551d01b63f57927671d485c',
  },
];

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

for (const sc of SCENARIOS) {
  describe.skipIf(sc.full && !FULL_TIER)(`${sc.device.toUpperCase()} ${FIRMWARE[sc.device]} ${sc.name}`, () => {
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
          roots: [...RT, sc.twin],
          symbols: SYMBOLS,
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
      expect(diff, ref ? "machine, parameter and page rows equal the static twin's" : "rows equal the no-module boot's").toEqual([]);
      if (ref)
        expect(
          Object.keys(got.machines).length + Object.keys(got.params).length + got.pages.length,
          'the run-time set added rows',
        ).toBeGreaterThan(0);
    }, 1_800_000);
  });
}
