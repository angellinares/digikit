import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { DkSession, EmuUsbTransport } from '@digi/dk-session';
import {
  repoRoot,
  withinRuntimeDeadline as beforeReadinessDeadline,
  type Device,
  type Session,
} from '@digi/test-support';
import { describe, expect, onTestFinished, test } from 'vitest';
import { expectRuntimeBoot, withPatchedSession, type PatchedSession } from '../support.ts';
import { timePhase, timingContext } from '../timing.ts';
import { planSymbol, prepareRuntimePlan, type RuntimePlan } from './runtime-plan.ts';

export const DEVICES = ['dt2', 'dn2'] as const;
export const FIRMWARE = { dt2: '1.17', dn2: '1.12' } as const;

export function liveOptions(device: Device, extra: { patchSet?: string; mirror?: boolean } = {}) {
  const binary = process.env[device === 'dt2' ? 'DIGI_EMUD_DT2' : 'DIGI_EMUD_DN2'];
  if (!binary) throw new Error(`DIGI_EMUD_${device.toUpperCase()} must name a preflighted daemon`);
  const card = device === 'dt2' ? path.join(repoRoot(), 'out/runs/patchkit/samples') : undefined;
  if (card && !fs.existsSync(card)) {
    throw new Error(
      `DT2 sample card missing: generate ${card} (tools.emulator.card_check.make_samples, run by pk_harness)`,
    );
  }
  return { device, firmware: FIRMWARE[device], binary, card, ...extra };
}

/** Full-tier scenarios run only under `DIGIKIT_TIER=full` (`verify --full`), as the Python tiers do. */
export const FULL_TIER = process.env.DIGIKIT_TIER === 'full';

/** One test per device, each on its own fresh runtime daemon that has passed `expectRuntimeBoot`. */
export function runtimeTests(
  name: string,
  body: (runtime: PatchedSession) => Promise<void>,
  opts: {
    patchSet?: string;
    mirror?: boolean;
    timeout?: number;
    devices?: readonly Device[];
    full?: boolean;
    skip?: string;
  } = {},
): void {
  for (const device of opts.devices ?? DEVICES) {
    describe.skipIf((opts.full === true && !FULL_TIER) || opts.skip !== undefined)(
      `${device.toUpperCase()} ${FIRMWARE[device]}`,
      () => {
        const handle = withPatchedSession(liveOptions(device, opts), name);
        test(
          name,
          async () => {
            await expectRuntimeBoot(handle.runtime);
            await timePhase('scenario.body', () => body(handle.runtime), timingContext(handle.runtime.session));
          },
          opts.timeout,
        );
      },
    );
  }
}

/** The DSP frame counter advances 1500 frames per emulated second. */
const FRAMES_PER_MS = 1.5;

/** Wait until the DSP has run `frames` more frames (emulated time). Wall time only guards against a stuck daemon. */
export async function waitEmulatedFrames(session: Session, frames: number, { hangMs = 30_000 } = {}): Promise<void> {
  const read = async () => Number(((await session.status()).audio as { frames?: number } | null)?.frames ?? 0);
  const start = await read();
  let seen = start;
  let lastAdvance = Date.now();
  while (seen < start + frames) {
    await delay(50);
    const now = await read();
    if (now > seen) lastAdvance = Date.now();
    else if (Date.now() - lastAdvance >= hangMs) {
      throw new Error(`DSP frames stuck at ${now} (started at ${start}, wanted ${start + frames}) for ${hangMs} ms`);
    }
    seen = now;
  }
}

/** Wait for `ms` of emulated time (the daemon must run freely). */
export function waitEmulated(session: Session, ms: number, opts: { hangMs?: number } = {}): Promise<void> {
  return waitEmulatedFrames(session, Math.ceil(ms * FRAMES_PER_MS), opts);
}

/** Wait for the runtime's normal UI hook on a fresh free-running daemon, before the first install. */
export async function expectRuntimeReady(session: Session): Promise<void> {
  const artifacts = liveArtifacts(session, 'runtime-ready');
  const plan = await prepareRuntimePlan(session, artifacts, ['DKRT_BOOT']);
  await timePhase('ready', () => expectRuntimeUi(session, plan, artifacts), timingContext(session));
}

/** Shared lifecycle deadline; preserve the existing readiness-helper export for its callers. */
export { beforeReadinessDeadline };

/** DkrtBootInfo word offsets, owned by modules/core/runtime/abi.toml (checked in modules/core/runtime/sdk/src/gen_abi.rs). */
const BOOT_FIELDS = { magic: 0, version: 4, safe: 8, reason: 12, counter: 16, count: 28, ui_reached: 36 } as const;

/** Current UI-hook evidence, rather than elapsed time or the boot's in-memory stability flag. */
export async function expectRuntimeUi(
  session: Session,
  plan: RuntimePlan,
  artifacts: string,
  options: { fresh?: boolean; afterEpoch?: number; timeout?: number; paused?: boolean } = {},
): Promise<number> {
  const timeout = options.timeout ?? 45_000;
  const deadline = Date.now() + timeout;
  const base = planSymbol(plan, 'DKRT_BOOT');
  let status: Awaited<ReturnType<Session['status']>> | undefined;
  let boot: Record<keyof typeof BOOT_FIELDS, number | null> | undefined;
  try {
    await expect
      .poll(
        async () => {
          status = await beforeReadinessDeadline(deadline, () => session.status());
          boot = Object.fromEntries(
            await beforeReadinessDeadline(deadline, () =>
              Promise.all(
                Object.entries(BOOT_FIELDS).map(async ([name, offset]) => [
                  name,
                  await session.client.memPeek(base + offset),
                ]),
              ),
            ),
          ) as typeof boot;
          return (
            status.mode === (options.paused ? 'paused' : 'free') &&
            // Restart preserves the card and relaxes the DT2 filesystem-worker check.
            // A new epoch still needs the current UI guards; an explicit filesystem failure is never accepted.
            (options.afterEpoch === undefined ? status.cf.ready : status.cf.filesystem_verified !== false) &&
            status.cf.main_ui_reached &&
            typeof status.epoch === 'number' &&
            (options.afterEpoch === undefined || status.epoch > options.afterEpoch) &&
            boot?.magic === 0x444b4254 &&
            boot.version === 1 &&
            boot.safe === 0 &&
            boot.reason === 0 &&
            boot.ui_reached === 1
          );
        },
        {
          timeout,
          interval: 500,
          message: 'current runtime boot reached the normal UI hook',
        },
      )
      .toBe(true);
    if (options.fresh) {
      // boot.rs only raises the persisted counter when count != 0 or counter != 0.
      // This fresh empty boot and one controlled install restart need no stability wait.
      expect(boot?.count, 'fresh pre-install boot has no selected modules').toBe(0);
      expect(boot?.counter, 'fresh pre-install boot found no failed boots').toBe(0);
    }
    if (typeof status?.epoch !== 'number') throw new Error('runtime status has no boot epoch');
    return status.epoch;
  } finally {
    record(artifacts, options.fresh ? 'readiness-pre-install' : 'readiness-post-install', {
      afterEpoch: options.afterEpoch,
      status: status ?? null,
      boot: boot ?? null,
      expired: Date.now() >= deadline,
    });
  }
}

/** Press, hold, release (also when the press fails), then let the UI settle. */
export async function tap(
  session: Session,
  send: (down: boolean) => Promise<unknown>,
  holdMs: number,
  settleMs = 1200,
): Promise<void> {
  try {
    await send(true);
    await waitEmulated(session, holdMs);
  } finally {
    await send(false);
  }
  await timePhase('settle.input', () => waitEmulated(session, settleMs));
}

export function record(dir: string, name: string, value: unknown): void {
  fs.writeFileSync(path.join(dir, `${name}.json`), JSON.stringify(value, null, 2));
}

/** Prepared separately with patchkit pack; acceptance never builds a toolchain, module or daemon. */
export function moduleArtifact(device: Device, id: string): string {
  const directory = process.env.DIGI_RUNTIME_MODULES ?? path.join(repoRoot(), 'out/regression/modules');
  const artifact = path.join(directory, device, `${id.split('/').at(-1)}.dkm`);
  if (!fs.existsSync(artifact)) throw new Error(`prepare ${id} for ${device} with patchkit pack: missing ${artifact}`);
  return artifact;
}

/** The debug module: the log (LOG, PEEK, LOGCFG) exists only while it is loaded, so a test that reads the log selects it next to the module it tests. */
export const DEBUG_MODULE = 'digikit/debug';

/** Native USB upload of prepared modules, one selection with all of them (in this order), one restart, then wait for their registration. */
export async function installModules(dk: DkSession, modules: { id: string; artifact: string }[]): Promise<void> {
  await timePhase(
    'module.install',
    async () => {
      const paths: string[] = [];
      for (const { id, artifact } of modules) {
        const bytes = fs.readFileSync(artifact);
        const storePath = `modules/${id.split('/').at(-1)}.dkm`;
        await timePhase('module.upload', () => dk.put(storePath, bytes));
        expect(await dk.verifyFile(storePath, bytes), 'fresh card read matches prepared module bytes').toBe(true);
        paths.push(storePath);
      }
      await dk.select(paths);
      await timePhase('protocol.restart', () => dk.restart());
      await timePhase('protocol.waitForBoot', () => dk.waitForBoot());
      await expect
        .poll(async () => (await dk.modules()).map((entry) => entry.name), { timeout: 30_000, interval: 500 })
        .toEqual(expect.arrayContaining(modules.map((m) => m.id)));
    },
    { detail: modules.map((m) => m.id).join(' ') },
  );
}

/** One prepared module alone (a test that does not read the log). */
export function installModule(dk: DkSession, id: string, artifact: string): Promise<void> {
  return installModules(dk, [{ id, artifact }]);
}

/** The debug module and the modules `ids`, in one selection and one restart. */
export function installWithDebug({ session, dk }: PatchedSession, ...ids: string[]): Promise<void> {
  const device = session.options.device;
  return installModules(
    dk,
    [...new Set([DEBUG_MODULE, ...ids])].map((id) => ({ id, artifact: moduleArtifact(device, id) })),
  );
}

/** New protocol and USB receive state; deliberately retain the daemon/card/boot history. */
export function liveProtocol(session: Session): DkSession {
  return new DkSession(new EmuUsbTransport((method, params) => session.client.call(method, params)), {
    timeoutMs: 20_000,
    chunk: 4096,
  });
}

const artifactDirectories = new WeakMap<Session, Map<string, string>>();

export function liveArtifacts(session: Session, workflow: string): string {
  let workflows = artifactDirectories.get(session);
  if (!workflows) {
    workflows = new Map();
    artifactDirectories.set(session, workflows);
  }
  const existing = workflows.get(workflow);
  if (existing) return existing;
  const root = path.join(os.tmpdir(), 'digi-regression', workflow, session.options.device);
  fs.mkdirSync(root, { recursive: true });
  const dir = fs.mkdtempSync(path.join(root, 'run-'));
  workflows.set(workflow, dir);
  console.info('runtime artifacts:', dir);
  try {
    // Removed when the requesting test passes; kept on failure, and when called outside a test.
    onTestFinished(({ task }) => {
      workflows.delete(workflow);
      if (task.result?.state === 'pass') fs.rmSync(dir, { recursive: true, force: true });
      else console.info('artifacts kept:', dir);
    });
  } catch {
    // Outside a test: keep the directory.
  }
  return dir;
}

export function liveCheck(name: string, ok: unknown, detail: unknown = ''): void {
  expect(Boolean(ok), `${name}: ${String(detail)}`).toBe(true);
}

/** 0.1 seconds at 44.1 kHz exercises the converter's resampling path. */
export function sampleWav(channels: 1 | 2): Uint8Array {
  const frames = 4410;
  const pcm = Buffer.alloc(frames * channels * 2);
  for (let i = 0; i < frames; i++) {
    pcm.writeInt16LE(Math.round(12000 * Math.sin((2 * Math.PI * 440 * i) / 44100)), i * channels * 2);
    if (channels === 2) pcm.writeInt16LE(Math.round(9000 * Math.sin((2 * Math.PI * 660 * i) / 44100)), i * 4 + 2);
  }
  const h = Buffer.alloc(44);
  h.write('RIFF', 0);
  h.writeUInt32LE(36 + pcm.length, 4);
  h.write('WAVEfmt ', 8);
  h.writeUInt32LE(16, 16);
  h.writeUInt16LE(1, 20);
  h.writeUInt16LE(channels, 22);
  h.writeUInt32LE(44100, 24);
  h.writeUInt32LE(44100 * channels * 2, 28);
  h.writeUInt16LE(channels * 2, 32);
  h.writeUInt16LE(16, 34);
  h.write('data', 36);
  h.writeUInt32LE(pcm.length, 40);
  return Uint8Array.from(Buffer.concat([h, pcm]));
}
