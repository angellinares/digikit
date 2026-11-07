import { execFile } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { promisify } from 'node:util';
import { expect } from 'vitest';
import { DkSession, EmuUsbTransport } from '@digi/dk-session';
import {
  button,
  MS,
  repoRoot,
  withinRuntimeDeadline,
  type Device,
  type RuntimeBaseline,
  type Session,
} from '@digi/test-support';
import { aggregate, withReusablePatchedSession, type PatchedSession } from '../support.ts';
import { record } from './artifacts.ts';
import { cachedCard, cardKey, fileSha } from './card-cache.ts';
import { plusdriveBinary } from './card-kit.ts';
import { FIRMWARE, claimRunDir } from './devices.ts';
import { moduleArtifact } from './install.ts';
import { expectRuntimeUi } from './ready.ts';
import { advance, analysisJson, bounded, evidence, options, pickSine, word } from './fx-live.ts';
import {
  planSymbol,
  prepareRuntimePlanOffline,
  validateRuntimePlan,
  type PreparedRuntimePlan,
  type RuntimePlan,
} from './runtime-plan.ts';
import { runtimeSafety, runtimeWatchEnv } from './write-safety.ts';

const exec = promisify(execFile);
const root = repoRoot();
export interface FxProfile {
  fx_slots: number;
  send_slots: number;
  track_type_word: number;
  value_classes: number;
  selection: { copy_a: number; copy_b: number };
  settings: { copy_a: number; copy_b: number };
  kit_areas: [number, number][];
  fx: {
    kit_ptr: number;
    array_off: number;
    page_first: number;
    value_indices: number[];
    template_percent: number;
    template_bipolar: number;
    template_mix: number;
    store_off: number;
    store_halfwords: number;
    mark_index: number;
    tail_off: number;
    track_page: number;
    sound_off: number;
    sound_stride: number;
    send_template: number;
    send_indices: number[];
  };
}
export interface FxCardPreset {
  /** Store paths (modules/m0.dkm, etc.) in boot order; every card contains the complete scenario inventory. */
  selection: string[];
  /** UTF-8 settings keys and hex-encoded byte values. */
  settings?: [string, string][];
}
export interface PreparedFxSessionOptions {
  image: {
    name: string;
    modules: string[];
    inventory: string[];
    symbols: string[];
    policy: 'default' | 'fx-kit' | 'fx-routes';
  };
  initial: string;
  presets: Record<string, FxCardPreset>;
  requireAudible?: boolean | ((runtime: PatchedSession) => boolean);
}

/** Legacy bounded panel tap, including release of every held chord key on failure. */
export async function tap(session: Session, key: string, settleMs = 300, held?: string): Promise<void> {
  try {
    if (held) {
      await bounded(() => session.client.inputButton(button(held), true));
      await advance(session, 5);
    }
    await bounded(() => session.client.inputTap(button(key), 20 * MS));
    await advance(session, 25);
  } finally {
    if (held) {
      await bounded(() => session.client.inputButton(button(held), false));
      await advance(session, 5);
    }
  }
  if (settleMs) await advance(session, settleMs);
}

/** Wait until the boot input has finished, so the run can pause. */
export async function drainInputs(session: Session, timeoutMs = 30_000): Promise<void> {
  await expect
    .poll(
      async () => {
        const status = await bounded(() => session.status());
        return { pending: status.cf.input_pending, scheduled: status.scheduled_inputs };
      },
      { timeout: timeoutMs, interval: 100, message: 'boot input has finished before pausing' },
    )
    .toEqual({ pending: 0, scheduled: 0 });
}

/** Preserve the exact decoded OLED image and hash for pixel comparisons. */
export async function shot(session: Session, artifacts: string, tag: string): Promise<string> {
  const frame = await bounded(() => session.client.framebufferGet({ hash: true, data: true }));
  if (!frame.sha256 || !frame.data_b64) throw new Error('route screenshot lacks OLED bytes or hash');
  const bytes = Buffer.from(frame.data_b64, 'base64');
  if (bytes.length !== 1024) throw new Error('route screenshot has an invalid OLED byte count');
  fs.writeFileSync(path.join(artifacts, `${tag}.oled`), bytes);
  record(artifacts, `${tag}-frame`, frame);
  return frame.sha256;
}

export async function readTextRows(session: Session, address: number, count: number, width: number): Promise<string[]> {
  if (!Number.isInteger(count) || count < 0 || !Number.isInteger(width) || width <= 0 || width % 4) {
    throw new Error('route text rows require a nonnegative count and a positive word-aligned width');
  }
  const result: string[] = [];
  for (let row = 0; row < count; row++) {
    const bytes = Buffer.alloc(width);
    for (let offset = 0; offset < width; offset += 4)
      bytes.writeUInt32BE(await word(session, address + row * width + offset), offset);
    result.push(bytes.toString('latin1').split('\0')[0]!);
  }
  return result;
}

/** Read protocol state while paused using the existing USB transport's legacy 400,000-instruction poll step. */
export async function withPausedFxProtocol<T>(
  runtime: PatchedSession,
  operation: (dk: DkSession) => Promise<T>,
): Promise<T> {
  const deadline = Date.now() + 30_000;
  const status = await withinRuntimeDeadline(deadline, () => runtime.session.status());
  if (status.mode !== 'paused') throw new Error('route protocol reads require a paused runtime');
  let active = true;
  const dk = new DkSession(
    new EmuUsbTransport(
      async <R>(method: string, params?: Record<string, unknown>) => {
        if (!active) throw new Error('route protocol is no longer active');
        const result = await withinRuntimeDeadline(deadline, () => runtime.session.client.call<R>(method, params));
        if (!active) throw new Error('route protocol is no longer active');
        return result;
      },
      { stepInstructions: 400_000 },
    ),
  );
  try {
    return await withinRuntimeDeadline(deadline, () => operation(dk));
  } finally {
    active = false;
    dk.cancelPending();
  }
}

/** Shared ready/boot observations; scenario assertions retain their own capacity and routing expectations. */
export async function inspectFxBoot(
  runtime: PatchedSession,
  plan: RuntimePlan,
  expectedNames: string[],
  artifacts: string,
  tag: string,
  expectedResults = expectedNames.map(() => 0),
  readiness: { afterEpoch?: number } = {},
) {
  const { session, dk } = runtime;
  validateRuntimePlan(plan, session);
  await bounded(() => session.client.call('pcm.tap', { action: 'start' }));
  await bounded(() => session.client.call('run.mode', { mode: 'free' }));
  await expectRuntimeUi(session, plan, artifacts, readiness);
  const boot = planSymbol(plan, 'DKRT_BOOT');
  let modules = await bounded(() => dk.modules());
  let results: number[] = [];
  await expect
    .poll(
      async () => {
        modules = await bounded(() => dk.modules());
        results = await Promise.all(expectedResults.map(async (_, i) => (await word(session, boot + 40 + i * 4)) | 0));
        return { names: modules.map((module) => module.name), results };
      },
      { timeout: 30_000, interval: 500 },
    )
    .toEqual({ names: expectedNames, results: expectedResults });
  const capacity = await bounded(() => dk.capacity());
  await drainInputs(session);
  await bounded(() => session.client.call('run.mode', { mode: 'paused' }));
  const fields = await Promise.all(Array.from({ length: 10 }, (_, i) => word(session, boot + i * 4)));
  expect(fields.slice(0, 4)).toEqual([0x444b4254, 1, 0, 0]);
  expect(fields.slice(7, 10)).toEqual([expectedResults.length, expectedNames.length, 1]);
  const status = await bounded(() => session.status());
  expect(status.mode).toBe('paused');
  expect(status.cf.input_pending).toBe(0);
  expect(status.scheduled_inputs).toBe(0);
  record(artifacts, tag, { fields, modules, results, capacity, status, load: session.loadResult });
  return { fields, modules, results, capacity, epoch: status.epoch };
}

/** DT2 retains the legacy selected-type readback and adjacent-list-position retries. */
export async function selectSine(runtime: PatchedSession, profile: FxProfile): Promise<void> {
  const hint = runtime.session.options.device === 'dt2' ? 7 : 5;
  const sine = (await withPausedFxProtocol(runtime, (dk) => dk.modules())).find(
    (module) => module.name === 'test/sine',
  );
  expect(sine, 'sine is registered').toBeDefined();
  const want = sine!.types[0];
  expect(want).toBeDefined();
  for (const downs of runtime.session.options.device === 'dt2'
    ? [hint, hint + 1, hint - 1, hint + 2, hint + 3]
    : [hint]) {
    await pickSine(runtime.session, downs);
    if (runtime.session.options.device !== 'dt2') return;
    if ((((await word(runtime.session, profile.track_type_word)) >>> 8) & 0xff) === want) return;
  }
  expect(((await word(runtime.session, profile.track_type_word)) >>> 8) & 0xff, 'selected DT2 sine type').toBe(want);
}

/** One exact image/plan/write-watch policy per owned fixture. Independent cards reload in the same daemon. */
export function withPreparedFxSession(device: Device, scope: string, config: PreparedFxSessionOptions) {
  if (!Object.hasOwn(config.presets, config.initial)) throw new Error(`unknown initial route preset ${config.initial}`);
  const { name, inventory, modules, symbols, policy } = config.image;
  if (policy !== 'default' && policy !== 'fx-kit' && policy !== 'fx-routes')
    throw new Error('prepared FX image requires an explicit safety policy');
  let prepared: PreparedRuntimePlan;
  let profile: FxProfile;
  let artifacts: string;
  let watchOutput: string;
  let closed = 0;
  const baselines = new Map<string, RuntimeBaseline>();
  const overlays: string[] = [];
  const epochs = new Set<number>();
  const rememberEpoch = (epoch: unknown) => {
    if (typeof epoch !== 'number') throw new Error('route status lacks epoch');
    epochs.add(epoch);
  };
  const collectEvidence = async (runtime: PatchedSession, tag: string, requireAudible?: boolean) => {
    rememberEpoch((await bounded(() => runtime.session.status())).epoch);
    const required =
      requireAudible ??
      (typeof config.requireAudible === 'function' ? config.requireAudible(runtime) : (config.requireAudible ?? true));
    await evidence(runtime, artifacts, tag, overlays, required);
  };
  const fixture = withReusablePatchedSession(
    {
      ...options(device),
      runDirClaim: () => claimRunDir(device),
      env: async () => {
        artifacts = fs.mkdtempSync(path.join(os.tmpdir(), `digi-fx-${name}-${device}-`));
        console.info('FX route safety artifacts:', artifacts);
        prepared = await prepareRuntimePlanOffline(
          { device, firmware: FIRMWARE[device] },
          artifacts,
          artifacts,
          symbols,
          { modules },
        );
        planSymbol(prepared.plan, 'DKRT_BOOT');
        profile = await analysisJson<FxProfile>(
          'tools.patchkit.pk_digikit_fxroutes',
          ['profile', '--device', device],
          artifacts,
          'profile',
        );
        watchOutput = path.join(artifacts, 'writewatch.jsonl');
        const env = runtimeWatchEnv(prepared.planFile, watchOutput);
        record(artifacts, 'watch-environment', env);
        return env;
      },
    },
    {
      prepareBaseline: async (session) => {
        rememberEpoch((await bounded(() => session.status())).epoch);
        const binary = plusdriveBinary();
        if (!fs.existsSync(binary)) throw new Error(`prepare prebuilt plusdrive separately: ${binary}`);
        for (const [tag, preset] of Object.entries(config.presets)) {
          if (!/^[a-zA-Z0-9_-]+$/.test(tag)) throw new Error(`invalid route preset directory ${tag}`);
          if (
            new Set(preset.selection).size !== preset.selection.length ||
            preset.selection.some((file) => !inventory.some((_, index) => file === `modules/m${index}.dkm`))
          )
            throw new Error(`invalid route preset selection ${tag}`);
          const files = inventory.map((id, index) => ({ name: `m${index}.dkm`, file: moduleArtifact(device, id) }));
          // The card is built in WORK (`<work>/card/card.img`).
          const produce = async (work: string) => {
            const seed = path.join(work, 'seed');
            const destination = path.join(seed, 'digikit', 'modules');
            fs.mkdirSync(destination, { recursive: true });
            for (const f of files) fs.copyFileSync(f.file, path.join(destination, f.name));
            fs.writeFileSync(path.join(seed, 'digikit', 'selection.txt'), preset.selection.join('\n') + '\n');
            const out = path.join(work, 'card', 'card.img');
            fs.mkdirSync(path.dirname(out));
            const built = await exec(binary, ['build', seed, '-o', out, '--device', device], {
              cwd: root,
              timeout: 30_000,
            });
            record(artifacts, `card-${tag}`, { stdout: built.stdout, stderr: built.stderr });
            if (preset.settings?.length) {
              const entries = path.join(work, 'settings.json');
              fs.writeFileSync(
                entries,
                JSON.stringify(preset.settings.map(([key, value_hex]) => ({ key, value_hex }))),
              );
              await analysisJson(
                'tools.patchkit.pk_digikit_fxroutes',
                ['seed-card', out, '--device', device, '--entries', entries],
                artifacts,
                `settings-${tag}`,
              );
            }
            return out;
          };
          // A card with settings goes through the Python seed-card tool, whose imports the key does not cover: it is built in
          // the run dir each time. Other presets are built once into the card cache: equal inputs give the same card (and snapshot).
          let cardImage: string;
          if (preset.settings?.length) {
            const work = path.join(session.runDir, tag);
            fs.rmSync(work, { recursive: true, force: true });
            fs.mkdirSync(work, { recursive: true });
            cardImage = await produce(work);
          } else {
            const key = cardKey(device, {
              kind: 'fx-route-preset',
              modules: files.map((f) => ({ name: f.name, sha: fileSha(f.file) })),
              selection: preset.selection,
            });
            cardImage = await cachedCard(key, produce);
          }
          const boot = planSymbol(prepared.plan, 'DKRT_BOOT');
          if (!prepared.preparedImage) throw new Error('explicit route plan lacks prepared image identity');
          baselines.set(tag, {
            cardImage,
            preparedImage: prepared.preparedImage,
            guard: { protect: [{ lo: boot, hi: boot + 8, label: 'runtime boot identity' }], allow_pc: [] },
          });
        }
        return baselines.get(config.initial)!;
      },
      finishCase: (runtime) => collectEvidence(runtime, `case-${++closed}`),
      afterClose: async () => {
        const results = await Promise.allSettled([
          (async () => {
            const report = runtimeSafety(device, watchOutput, overlays, epochs.size, policy);
            record(artifacts, 'lifetime-safety', report);
            expect(report.pass, 'every route emulator and exported overlay meets legacy safety policy').toBe(true);
          })(),
          analysisJson('tools.patchkit.pk_digikit_fxroutes', ['render-shots', artifacts], artifacts, 'rendered-shots'),
        ]);
        const errors = results.filter((result) => result.status === 'rejected').map((result) => result.reason);
        if (errors.length === 1) throw errors[0];
        if (errors.length > 1) throw aggregate(errors, 'route lifetime safety and screenshot rendering failed');
      },
    },
    scope,
  );
  return {
    fixture,
    get plan() {
      return prepared.plan;
    },
    get artifacts() {
      return artifacts;
    },
    get profile() {
      return profile;
    },
    get slots() {
      return profile.fx_slots;
    },
    get sendSlots() {
      return profile.send_slots;
    },
    baseline(tag: string): RuntimeBaseline {
      const baseline = baselines.get(tag);
      if (!baseline) throw new Error(`unknown or unprepared route preset ${tag}`);
      return baseline;
    },
    rememberEpoch,
    evidence: collectEvidence,
  };
}
