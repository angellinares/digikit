import { execFile } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { promisify } from 'node:util';
import { readSettingsRecordIfPresent, settingsValue, type DkModule } from '@digi/dk-session';
import {
  button,
  emulatorClock,
  pollUntil,
  repoRoot,
  type Session,
  runUntilFramesWithProgress,
  stepWithProgress,
  waitClock,
  withinRuntimeDeadline,
  type Device,
  type RuntimeBaseline,
} from '@digi/test-support';
import { expect } from 'vitest';
import { expectNoGuardHits, withReusablePatchedSession, type PatchedSession, type TestRuntime } from '../support.ts';
import { record } from './artifacts.ts';
import { plusdriveBinary } from './card-kit.ts';
import { cachedCard, cardKey, fileSha } from './card-cache.ts';
import { daemonBinary, FIRMWARE, claimRunDir } from './devices.ts';
import { moduleArtifact } from './install.ts';
import { boundService, neededServices, SERVICE_MODULES } from './service-files.ts';
import { expectRuntimeUi } from './ready.ts';
import { deviceProfile } from './device-data.ts';
import type { FxExpectations } from './dkm-parse.ts';
import { committedFxExpectations } from './pack-needs.ts';
import { planSymbol, prepareRuntimePlanOffline, validateRuntimePlan, type RuntimePlan } from './runtime-plan.ts';
import { runtimeSafety, runtimeWatchEnv } from './write-safety.ts';
import { GAIN, SINE } from './fixtures.ts';
import { drainInputs } from './fx-session.ts';
import { moduleResultsNote } from './runtime-menu.ts';
import { pollEmulated } from './waits.ts';

const exec = promisify(execFile);
const root = repoRoot();
/** What the panel helpers need of a runtime: the session, the wait API's `ui` and its clock. */
export type Rt = Pick<TestRuntime, 'session' | 'ui' | 'clock'>;
/** The reason of a fixed wait after a panel input: the firmware redraws, and no page-change event exists. */
export const REDRAW = 'firmware redraw after a panel input (no page-change event)';
/** The reason of a fixed wait for the firmware to act on its own (a timer, a retry, a save): nothing observable yet. */
export const SETTLE = 'emulated time for the firmware to act (no observable state)';
// core/fx's published registry row: modules/core/adapters/fx/module.toml.
export const ROW_WORDS = 24;
export const FREE_PC = 0xffffffff;
export const bounded = <T>(operation: () => Promise<T>) => withinRuntimeDeadline(Date.now() + 30_000, operation);
export const rpc = <T>(session: Session, method: string, params?: Record<string, unknown>) =>
  bounded(() => session.client.call<T>(method, params));

export function options(device: Device) {
  return { device, firmware: FIRMWARE[device], binary: daemonBinary(device), autoNo: true };
}

export function slotsFor(device: Device): number {
  return deviceProfile(device).limits.fx_slots;
}

/** Native plusdrive owns all card/store/selection encoding. The cards are built once into the card cache and reused by their inputs. */
async function baseline(
  session: Session,
  plan: RuntimePlan,
  wet: boolean,
  artifacts: string,
): Promise<RuntimeBaseline> {
  const tag = wet ? 'wet' : 'dry';
  const binary = plusdriveBinary();
  if (!fs.existsSync(binary))
    throw new Error(
      `prepare prebuilt plusdrive separately: ${binary}; this fixture also needs prepared sine and gain DKMs`,
    );
  const device = session.options.device;
  const files = [SINE, GAIN].map((id) => {
    try {
      return { name: `${id.split('/')[1]}.dkm`, file: moduleArtifact(device, id) };
    } catch (error) {
      throw new Error(`FX reuse needs prepared ${id} and the prebuilt native plusdrive card builder; ${String(error)}`);
    }
  });
  const selection = ['modules/sine.dkm', ...(wet ? ['modules/dyn-fx-gain.dkm'] : [])];
  // the services the gain effect needs (fx.dks), bound to the stock firmware, go to digikit/services
  const services = await Promise.all(
    neededServices(files.map((f) => f.file)).map(async (name) => ({
      name: `${name}.dks`,
      file: await boundService(device, name, moduleArtifact(device, SERVICE_MODULES[name]!)),
    })),
  );
  const key = cardKey(device, {
    kind: 'fx-live-reuse',
    modules: files.map((f) => ({ name: f.name, sha: fileSha(f.file) })),
    services: services.map((f) => ({ name: f.name, sha: fileSha(f.file) })),
    selection,
  });
  const cardImage = await cachedCard(key, async (work) => {
    // The seed stays in the card dir: tests read the built modules from it.
    const seed = path.join(work, 'card', 'seed');
    const modules = path.join(seed, 'digikit', 'modules');
    fs.mkdirSync(modules, { recursive: true });
    for (const f of files) fs.copyFileSync(f.file, path.join(modules, f.name));
    const serviceDir = path.join(seed, 'digikit', 'services');
    fs.mkdirSync(serviceDir, { recursive: true });
    for (const f of services) fs.copyFileSync(f.file, path.join(serviceDir, f.name));
    fs.writeFileSync(path.join(seed, 'digikit', 'selection.txt'), selection.join('\n') + '\n');
    const out = path.join(work, 'card', 'card.img');
    fs.mkdirSync(path.dirname(out), { recursive: true });
    const built = await exec(binary, ['build', seed, '-o', out, '--device', device], { cwd: root, timeout: 30_000 });
    record(artifacts, `card-${tag}`, { stdout: built.stdout, stderr: built.stderr });
    return out;
  });
  const boot = planSymbol(plan, 'DKRT_BOOT');
  return {
    cardImage,
    // A targeted run-time invariant guard, not the legacy launch-time expansion write-watch/flush proof.
    guard: { protect: [{ lo: boot, hi: boot + 8, label: 'runtime boot identity' }], allow_pc: [] },
  };
}

/** The frame hash once the screen holds still: two reads 100 ms of emulated time apart are equal (a redraw may follow the first tap after a boot). */
export async function stableFrameHash(rt: Rt): Promise<string> {
  let last: string | undefined;
  const { hash } = await pollUntil(
    rt.clock,
    async () => {
      const hash = await rt.session.frameHash();
      const stable = hash === last;
      last = hash;
      return { hash, stable };
    },
    (r) => r.stable,
    { withinMs: 5000, intervalMs: 100, message: 'the frame holds still for 100 ms of emulated time' },
  );
  return hash;
}

export async function analysisJson<T>(module: string, args: string[], artifacts: string, tag: string): Promise<T> {
  try {
    const result = await exec('uv', ['run', '--no-sync', 'python', '-m', module, ...args], {
      cwd: root,
      timeout: 180_000, // a heavy analysis (four 28 MB overlays) takes 8 s on an idle host and over 30 s under load: the wall time only guards against a hang
      maxBuffer: 4 * 1024 * 1024,
      env: { ...process.env, UV_NO_SYNC: '1' },
    });
    const value = JSON.parse(result.stdout) as T;
    record(artifacts, tag, value);
    return value;
  } catch (error) {
    const output = error as { stdout?: string; stderr?: string };
    record(artifacts, tag, { error: String(error), stdout: output.stdout, stderr: output.stderr });
    throw error;
  }
}

export function expectGainRow(row: number[], oracle: FxExpectations): void {
  expect(row.slice(0, 12), 'exact process/descriptor/state/default words').toEqual(oracle.row);
  expect(row[12], 'adapter ran the effect').toBe(1);
  expect(row[14], 'effect is not disabled').toBe(0);
  expect(row[17], 'no effect fault').toBe(0);
}

export async function word(session: Session, address: number): Promise<number> {
  const value = await bounded(() => session.client.memPeek(address));
  if (value === null) throw new Error(`unmapped CF word at ${address.toString(16)}`);
  return value >>> 0;
}

export async function dspBytes(session: Session, address: number, length: number): Promise<Buffer> {
  session.requireDsp('dspBytes');
  const result = await rpc<{ hex: string; unknown: number }>(session, 'dsp.peek', { addr: address, len: length });
  expect(result.unknown, 'DSP bytes are mapped').toBe(0);
  const bytes = Buffer.from(result.hex, 'hex');
  expect(bytes.length).toBe(length);
  return bytes;
}

export async function dspWords(session: Session, address: number, count: number): Promise<number[]> {
  const bytes = await dspBytes(session, address, count * 4);
  return Array.from({ length: count }, (_, i) => bytes.readUInt32LE(i * 4));
}

export async function rows(session: Session, plan: RuntimePlan, slots: number): Promise<number[][]> {
  const words = await dspWords(session, planSymbol(plan, 'fx.rows'), ROW_WORDS * slots);
  return Array.from({ length: slots }, (_, i) => words.slice(i * ROW_WORDS, (i + 1) * ROW_WORDS));
}

export async function frames(session: Session): Promise<number> {
  session.requireDsp('frames (the DSP frame count)');
  const frame = (await bounded(() => session.status())).dsp_frames;
  if (typeof frame !== 'number') throw new Error('status lacks DSP frame count');
  return frame;
}

/** Run to absolute DSP frame `target`; fails only when a half emulated second makes no progress for PROGRESS_HANG_S. */
export async function runToFrame(session: Session, target: number): Promise<void> {
  await runUntilFramesWithProgress(session.client, () => frames(session), target);
}

/** Run `n` more DSP frames with the same progress guard. */
export async function runFrames(session: Session, n: number): Promise<void> {
  await runToFrame(session, (await frames(session)) + n);
}

/** Choose the original device list hint or a numeric override without asserting selected-track identity.
 * The BASIC chain uses selectSine for DT2 type readback; gain/deselect retains its audible-input contract. */
export async function pickSine(rt: Rt, selection?: number, ups = 0): Promise<void> {
  const hint = selection ?? (rt.session.options.device === 'dt2' ? 7 : 5);
  if (!Number.isInteger(hint) || hint < 0) throw new Error('FX input hint must be a nonnegative list position');
  await rt.ui.tap('SRC', { until: { drained: true }, held: 'FUNC' });
  await rt.ui.wait({ ms: 200, why: REDRAW });
  // `ups` UP presses first: every try starts from the top of the list
  for (let i = 0; i < ups; i++) {
    await rt.ui.tap('UP', { until: { drained: true } });
    await rt.ui.wait({ ms: 80, why: REDRAW });
  }
  for (let i = 0; i < hint; i++) {
    await rt.ui.tap('DOWN', { until: { drained: true } });
    await rt.ui.wait({ ms: 80, why: REDRAW });
  }
  await rt.ui.tap('YES', { until: { drained: true } });
  await rt.ui.wait({ ms: 300, why: REDRAW });
  await rt.ui.tap('YES', { until: { drained: true } });
  await rt.ui.wait({ ms: 300, why: REDRAW });
}

export interface NoteOptions {
  /** WAV file for the PCM capture. */
  file: string;
  /** Absolute DSP frame at which the note starts (after the block boundary). */
  trigger: number;
  /** Names the setup error and the fault check. */
  tag: string;
  /** Frames captured after the trigger. */
  length?: number;
  /** `pcm.tap start` was already sent by the caller. */
  tapStarted?: boolean;
  /** Skip the 44 000-instruction offset after the block boundary. */
  skipStep?: boolean;
  /** Capture without pressing TRIG 1. */
  noPress?: boolean;
  /** Runs once after the key is down, inside the capture. */
  during?: () => Promise<void>;
}

/** The shared note sequence: run to `trigger`, capture `length` frames with TRIG 1 held, then check for faults. */
export async function captureNote(session: Session, opts: NoteOptions): Promise<void> {
  const { file, trigger, tag, length = 1500 } = opts;
  session.requireDsp('captureNote');
  const current = await frames(session);
  if (current >= trigger - 20) throw new Error(`${tag} setup too late: current frame ${current}, trigger ${trigger}`);
  if (!opts.tapStarted) await rpc(session, 'pcm.tap', { action: 'start' });
  try {
    await runToFrame(session, trigger);
    if (!opts.skipStep) await stepWithProgress(session.client, 44_000);
    await rpc(session, 'pcm.tap', { action: 'capture', path: file });
    if (!opts.noPress) await bounded(() => session.client.inputButton(button('TRIG 1'), true));
    await opts.during?.();
    await runToFrame(session, trigger + length);
  } finally {
    try {
      await rpc(session, 'pcm.tap', { action: 'capture_stop' });
    } finally {
      await bounded(() => session.client.inputButton(button('TRIG 1'), false));
    }
  }
  await waitClock(emulatorClock(session), 50); // the last frames of the note reach the capture
  await bounded(() => session.expectNoFault(tag));
}

/** Future-frame choice follows the legacy restarted-note path; boundary offset and 1500-frame hold are unchanged. */
export async function capture(runtime: Rt, file: string, selection?: number, triggerFrame?: number): Promise<number> {
  const { session } = runtime;
  await rpc(session, 'run.mode', { mode: 'paused' });
  await rpc(session, 'pcm.tap', { action: 'start' });
  await pickSine(runtime, selection);
  const current = await frames(session);
  const trig = triggerFrame ?? current + 300;
  record(path.dirname(file), `${path.basename(file, path.extname(file))}-timing`, {
    currentFrame: current,
    triggerFrame: trig,
  });
  await captureNote(session, { file, trigger: trig, tag: 'FX note', tapStarted: true });
  return trig;
}

export interface PcmMetrics {
  ok: boolean;
  identical: boolean;
  rms_db: number | null;
  rms_base: number;
  shift: number | null;
}
/** The `hook_ab compare` metrics of a render against the reference scaled by `gain`, from the onset on. The CLI exits 1 on a failed verdict; its JSON on stdout is still read. */
export async function pcmMetrics(
  base: string,
  sample: string,
  gain: number,
  artifacts: string,
  tag: string,
  relBound?: number,
): Promise<PcmMetrics> {
  const args = ['run', '--no-sync', 'python', '-m', 'tools.patchkit.hook_ab', 'compare', base, sample];
  args.push('--gain', String(gain), '--align-onset', '--json');
  if (relBound !== undefined) args.push('--rel-bound', String(relBound));
  let stdout: string;
  try {
    stdout = (await exec('uv', args, { cwd: root, timeout: 30_000 })).stdout;
  } catch (error) {
    const output = error as { stdout?: string; stderr?: string };
    if (!output.stdout) {
      record(artifacts, `pcm-${tag}`, { error: String(error), stderr: output.stderr });
      throw error;
    }
    stdout = output.stdout;
  }
  const metrics = JSON.parse(stdout) as PcmMetrics;
  record(artifacts, `pcm-${tag}`, metrics);
  return metrics;
}

/**
 * Asserts that a render equals the reference scaled by `gain`. The CLI verdict (`ok`: the largest error within
 * `relBound` of the scaled reference peak, 2**-20 when unset) is the only error rule; this adds audibility, the gain
 * in dB and the onset shift (hook_ab counts interleaved samples; the tolerance is 128 audio frames).
 */
export async function compare(
  base: string,
  sample: string,
  gain: number,
  artifacts: string,
  tag: string,
  identical = false,
  relBound?: number,
): Promise<PcmMetrics> {
  const metrics = await pcmMetrics(base, sample, gain, artifacts, tag, relBound);
  expect(metrics.rms_base, 'reference has sound').toBeGreaterThan(0);
  expect(metrics.ok, 'PCM equals the scaled reference within the error bound').toBe(true);
  expect(metrics.rms_db).not.toBeNull();
  expect(Math.abs(metrics.rms_db! - 20 * Math.log10(gain))).toBeLessThan(0.001);
  expect(metrics.shift).not.toBeNull();
  expect(Math.abs(metrics.shift!) / 2).toBeLessThanOrEqual(128);
  if (identical) expect(metrics.identical, 'dry PCM is bit-identical from onset').toBe(true);
  return metrics;
}

/** Interleaved samples of an IEEE-float WAV (format 3). */
export function readFloatWav(file: string): Float32Array {
  const d = fs.readFileSync(file);
  if (d.toString('latin1', 0, 4) !== 'RIFF' || d.toString('latin1', 8, 12) !== 'WAVE')
    throw new Error(`${file}: not a WAV`);
  let pos = 12;
  let format = 0;
  while (pos + 8 <= d.length) {
    const tag = d.toString('latin1', pos, pos + 4);
    const size = d.readUInt32LE(pos + 4);
    if (tag === 'fmt ') format = d.readUInt16LE(pos + 8);
    else if (tag === 'data') {
      if (format !== 3) throw new Error(`${file}: expected an IEEE float WAV, format ${format}`);
      const out = new Float32Array(Math.floor(Math.min(size, d.length - pos - 8) / 4));
      for (let i = 0; i < out.length; i++) out[i] = d.readFloatLE(pos + 8 + 4 * i);
      return out;
    }
    pos += 8 + size + (size & 1);
  }
  throw new Error(`${file}: no data chunk`);
}

/** (onset of `run` minus onset of `ref`, number of samples that differ from the onsets on over the common length); the onset is the first sample above 1e-6. */
export function alignedCompare(ref: Float32Array, run: Float32Array) {
  const onset = (x: Float32Array) => x.findIndex((v) => Math.abs(v) > 1e-6);
  const a = onset(ref);
  const b = onset(run);
  if (a < 0 || b < 0) return { shift: null, differ: -1 };
  const n = Math.min(ref.length - a, run.length - b);
  let differ = 0;
  for (let i = 0; i < n; i++) if (ref[a + i] !== run[b + i]) differ++;
  return { shift: b - a, differ };
}

/** Equal, or equal sample for sample from the onsets on with an even onset shift of at most `maxShift` samples. */
export function pcmMatches(ref: Float32Array, run: Float32Array, maxShift: number) {
  const exact = ref.length === run.length && ref.every((v, i) => v === run[i]);
  const { shift, differ } = alignedCompare(ref, run);
  const ok = exact || (shift !== null && shift % 2 === 0 && Math.abs(shift) <= maxShift && differ === 0);
  return {
    ok,
    detail: `${run.length} samples, exact ${exact}, onset shift ${shift}, ${differ} differ after alignment`,
  };
}

export const peak = (x: Float32Array): number => x.reduce((m, v) => Math.max(m, Math.abs(v)), 0);

/** RMS of each full window of `win` samples. */
export function windowRms(samples: Float32Array, win = 4800): number[] {
  const out: number[] = [];
  for (let i = 0; i + win < samples.length; i += win) {
    let sum = 0;
    for (let k = i; k < i + win; k++) sum += samples[k]! * samples[k]!;
    out.push(Math.sqrt(sum / win));
  }
  return out;
}

/** The onset (first sample above 1e-6, -1 for none) and the little-endian float32 bytes of `n` interleaved samples from it (Python `pcm_pin`). */
export function pcmWindow(pcm: Float32Array, n: number): { onset: number; window: Buffer } {
  const onset = pcm.findIndex((x) => Math.abs(x) > 1e-6);
  const part = onset >= 0 ? pcm.subarray(onset, onset + n) : pcm.subarray(0, 0);
  const window = Buffer.alloc(part.length * 4);
  part.forEach((x, i) => window.writeFloatLE(x, 4 * i));
  return { onset, window };
}

/**
 * Play TRIG 1 from DSP frame `trigger` (the same instruction after the block boundary in every run) and capture 1500 frames to `file`. `during`
 * runs once after the key is down, inside the capture. The key stays down for the whole capture. Python `Rig.note`.
 */
export async function playNote(session: Session, file: string, trigger: number, during?: () => Promise<void>) {
  await captureNote(session, { file, trigger, tag: 'note', during });
}

/** Gain-fixture assertions only: fixed sine/gain module and default-emulator assumptions. */
export async function inspect(
  runtime: PatchedSession,
  plan: RuntimePlan,
  slots: number,
  wet: boolean,
  artifacts: string,
  tag: string,
  afterEpoch?: number,
) {
  const { session, dk } = runtime;
  validateRuntimePlan(plan, session);
  await rpc(session, 'run.mode', { mode: 'free' });
  await expectRuntimeUi(session, plan, artifacts, { afterEpoch });
  const names = [SINE, ...(wet ? [GAIN] : [])];
  let modules: DkModule[] = [];
  let results: number[] = [];
  try {
    await pollEmulated(
      session,
      async () => {
        modules = await bounded(() => dk.modules());
        const boot = planSymbol(plan, 'DKRT_BOOT');
        results = await Promise.all(names.map((_, i) => word(session, boot + 40 + i * 4)));
        return { names: modules.map((module) => module.name), results };
      },
      (state) => JSON.stringify(state) === JSON.stringify({ names, results: names.map(() => 0) }),
      { emulatedMs: 30_000, intervalMs: 500, message: 'the expected modules and results' },
    );
  } catch (error) {
    throw new Error(`${(error as Error).message}${await moduleResultsNote(session, plan, results)}`);
  }
  const capacity = await bounded(() => dk.capacity());
  expect(capacity.FX_SLOTS).toEqual({ limit: slots, used: wet ? 1 : 0, largest: 0 });
  const settings = await bounded(() => readSettingsRecordIfPresent(dk));
  expect(settings === null ? null : settingsValue(settings, 'dk/fault')).toBeNull();
  await drainInputs(session);
  await rpc(session, 'run.mode', { mode: 'paused' });
  const boot = planSymbol(plan, 'DKRT_BOOT');
  const fields = await Promise.all(Array.from({ length: 10 + names.length }, (_, i) => word(session, boot + i * 4)));
  // modules/core/runtime/abi.toml DkrtBootInfo: magic/version/safe/reason, count/loaded/UI, results.
  expect(fields.slice(0, 4)).toEqual([0x444b4254, 1, 0, 0]);
  expect(fields.slice(7, 10)).toEqual([names.length, names.length, 1]);
  expect(fields.slice(10)).toEqual(names.map(() => 0));
  const assigned = planSymbol(plan, 'DKRT_ASSIGN');
  expect(await word(session, assigned)).toBe(0x444b4153);
  expect(await word(session, assigned + 4)).toBe(1);
  expect(modules[0]!.types).toHaveLength(1);
  const chain = await rows(session, plan, slots);
  expect(chain.slice(wet ? 1 : 0).map((row) => row[0])).toEqual(Array(slots - (wet ? 1 : 0)).fill(FREE_PC));
  let oracle: FxExpectations | undefined;
  if (wet) {
    const effect = modules[1]!;
    expect(effect.needs, 'the effect module needs the runtime of sdk/sdk.toml').toBeGreaterThan(0);
    expect(effect.rows).toBe(0);
    const base = planSymbol(plan, '_dl_area') + effect.dsp_off;
    oracle = committedFxExpectations(session.options.device, GAIN, base); // the module that the card was built from
    record(artifacts, `${tag}-expected-gain`, oracle);
    expect(effect.dsp_len, 'MODULES range agrees with independently inferred needs').toBe(oracle.needs.DSP_AREA);
    expect(oracle.needs.FX_SLOTS).toBe(1);
    expect(capacity.FX_SLOTS!.used).toBe(oracle.needs.FX_SLOTS);
    expect(
      (await dspBytes(session, base, oracle.image_hex.length / 2)).toString('hex'),
      'independent relocated DSP image',
    ).toBe(oracle.image_hex);
    expectGainRow(chain[0]!, oracle);
  }
  const status = await bounded(() => session.status());
  expect(status.cf.main_ui_reached).toBe(true);
  expect(status.cf.input_pending).toBe(0);
  expect(status.scheduled_inputs).toBe(0);
  expect(status.dsp).toBe('sharc');
  const semantic = {
    boot: { count: fields[7], loaded: fields[8], safe: fields[2], counter: fields[4] },
    modules: modules.map(({ name, needs, types, rows: count, dsp_len }) => ({
      name,
      needs,
      types,
      rows: count,
      dsp_len,
    })),
    capacity: capacity.FX_SLOTS,
    freeRows: chain.filter((row) => row[0] === FREE_PC).length,
    settings: settings === null ? null : Buffer.from(settings).toString('hex'),
    input: { pending: status.cf.input_pending, scheduled: status.scheduled_inputs },
    dsp: status.dsp,
  };
  record(artifacts, tag, { semantic, modules, chain, status, capacity });
  return { semantic, chain, oracle, epoch: status.epoch };
}

export async function evidence(
  runtime: PatchedSession,
  artifacts: string,
  tag: string,
  overlays: string[],
  requireAudible?: boolean,
): Promise<void> {
  const { session } = runtime;
  // Capture the guard report and the overlay first, so that a fault still leaves its artifacts.
  const guard = await bounded(() => session.client.guardReport());
  const file = path.join(artifacts, `${tag}.overlay.bin`);
  const overlay = await rpc(session, 'card.overlay', { path: file });
  overlays.push(file);
  // A case in which no tap ran (for example one a test replaced by a reload before it played anything) has no PCM
  // evidence: guard, overlay and fault checks still run, unless the case requires audio. After a recorded tap start,
  // a failing report fails the case.
  const pcm = session.pcmTapStarted
    ? await rpc<{ non_finite: number; peak: number }>(session, 'pcm.tap', { action: 'report' })
    : undefined;
  record(artifacts, `${tag}-closing`, { guard, overlay, pcm, status: await bounded(() => session.status()) });
  await bounded(() => session.expectNoFault(tag));
  await bounded(() => expectNoGuardHits(session));
  if (!pcm && requireAudible) throw new Error(`${tag}: the case requires audio but no PCM tap ran`);
  if (pcm) {
    expect(pcm.non_finite).toBe(0);
    if (requireAudible) expect(pcm.peak, 'input is audible').toBeGreaterThan(0.01);
  }
}

/** The per-case evidence closer of a fixture: remembers the epoch, then checks evidence, fault, guard and PCM. */
export function evidenceCollector(
  config: { requireAudible?: RequireAudible },
  artifacts: () => string,
  overlays: string[],
  rememberEpoch: (epoch: unknown) => void,
  close: typeof evidence = evidence,
) {
  return async (runtime: PatchedSession, tag: string, requireAudible?: boolean): Promise<void> => {
    rememberEpoch((await bounded(() => runtime.session.status())).epoch);
    await close(runtime, artifacts(), tag, overlays, audibleRequired(config, runtime, requireAudible));
  };
}

/** Audio is required only when the call (`override`) or the suite sets `requireAudible`. */
export type RequireAudible = boolean | ((runtime: PatchedSession) => boolean);
export function audibleRequired(
  config: { requireAudible?: RequireAudible },
  runtime: PatchedSession,
  override?: boolean,
): boolean {
  const { requireAudible } = config;
  return override ?? (typeof requireAudible === 'function' ? requireAudible(runtime) : (requireAudible ?? false));
}

export interface GainFxSessionOptions {
  initial?: 'dry' | 'wet';
  /** A patch set of the device file other than `emulator` (the sine and gain DKMs load as in any case). */
  patchSet?: string;
  /** Idle cases may be silent; guard, nonfinite, fault and overlay checks always remain mandatory. A suite that needs sound sets `true`. */
  requireAudible?: RequireAudible;
}

/** Fixed default-emulator sine/gain fixtures, sharing the existing owned Session lifecycle. */
export function withGainFxSession(device: Device, scope: string, config: GainFxSessionOptions = {}) {
  let plan: RuntimePlan;
  let dry: RuntimeBaseline;
  let wet: RuntimeBaseline;
  let artifacts: string;
  let closed = 0;
  let watchOutput: string;
  const overlays: string[] = [];
  const epochs = new Set<number>();
  const rememberEpoch = (epoch: unknown) => {
    if (typeof epoch !== 'number') throw new Error('runtime status lacks epoch');
    epochs.add(epoch);
  };
  const slots = slotsFor(device);
  const collectEvidence = evidenceCollector(config, () => artifacts, overlays, rememberEpoch);
  const fixture = withReusablePatchedSession(
    {
      ...options(device),
      dsp: 'sharc', // effects run on the DSP
      patchSet: config.patchSet,
      runDirClaim: () => claimRunDir(device),
      env: async () => {
        artifacts = fs.mkdtempSync(path.join(os.tmpdir(), `digi-fx-reuse-${device}-`));
        console.info('FX safety artifacts:', artifacts);
        const prepared = await prepareRuntimePlanOffline(
          { device, firmware: FIRMWARE[device], runtime: { patchSet: config.patchSet } },
          artifacts,
          artifacts,
          ['DKRT_BOOT', 'DKRT_ASSIGN', 'fx.rows', '_dl_area'],
        );
        plan = prepared.plan;
        watchOutput = path.join(artifacts, 'writewatch.jsonl');
        const env = runtimeWatchEnv(prepared.planFile, watchOutput);
        record(artifacts, 'watch-environment', env);
        return env;
      },
    },
    {
      prepareBaseline: async (session) => {
        rememberEpoch((await bounded(() => session.status())).epoch);
        dry = await baseline(session, plan, false, artifacts);
        wet = await baseline(session, plan, true, artifacts);
        return config.initial === 'wet' ? wet : dry;
      },
      finishCase: (runtime) => collectEvidence(runtime, `case-${++closed}`),
      tempDirs: () => [artifacts],
      afterClose: async () => {
        const report = runtimeSafety(device, watchOutput, overlays, epochs.size);
        record(artifacts, 'lifetime-safety', report);
        expect(report.pass, 'every emulator and exported overlay meets legacy safety policy').toBe(true);
      },
    },
    scope,
  );
  return {
    fixture,
    get plan() {
      return plan;
    },
    get artifacts() {
      return artifacts;
    },
    get slots() {
      return slots;
    },
    get dry() {
      return dry;
    },
    get wet() {
      return wet;
    },
    rememberEpoch,
    evidence: collectEvidence,
  };
}
