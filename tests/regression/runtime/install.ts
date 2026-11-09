import fs from 'node:fs';
import path from 'node:path';
import { repoRoot, type Device, type Session } from '@digi/test-support';
import { expect } from 'vitest';
import { type PatchedSession } from '../support.ts';
import { timePhase } from '../timing.ts';
import { liveArtifacts } from './artifacts.ts';
import { expectRuntimeUi } from './ready.ts';
import { prepareRuntimePlan, type RuntimePlan } from './runtime-plan.ts';

type Rig = Pick<PatchedSession, 'session' | 'dk'>;

const bootPlans = new WeakMap<Session, RuntimePlan>();

/** The main UI of the boot after a restart (an install ends with one): a new epoch, and the DT2 filesystem check is the relaxed one that a restart needs. */
export async function expectUiAfterRestart(session: Session, epochBefore: number | undefined): Promise<void> {
  const artifacts = liveArtifacts(session, 'install-ready');
  let plan = bootPlans.get(session);
  if (!plan) {
    plan = await prepareRuntimePlan(session, artifacts, ['DKRT_BOOT']);
    bootPlans.set(session, plan);
  }
  await expectRuntimeUi(session, plan, artifacts, { afterEpoch: epochBefore ?? -1 });
}

/** Prepared separately with patchkit pack; acceptance never builds a toolchain, module or daemon. */
export function moduleArtifact(device: Device, id: string): string {
  const directory = process.env.DIGI_RUNTIME_MODULES ?? path.join(repoRoot(), 'dist/modules');
  const artifact = path.join(directory, device, `${id.split('/').at(-1)}.dkm`);
  if (!fs.existsSync(artifact)) throw new Error(`missing ${artifact}: run nx run ${id}:pack`);
  return artifact;
}

/** The debug module: the log (LOG, PEEK, LOGCFG) exists only while it is loaded, so a test that reads the log selects it next to the module it tests. */
export const DEBUG_MODULE = 'digikit/debug';

/** Native USB upload of prepared modules, one selection with all of them (in this order), one restart, then wait for their registration. */
export async function installModules({ session, dk }: Rig, modules: { id: string; artifact: string }[]): Promise<void> {
  const epochBefore = (await session.status()).epoch;
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
  // dk.waitForBoot only sees the restart event: a test that sends keys next needs the main UI.
  await expectUiAfterRestart(session, typeof epochBefore === 'number' ? epochBefore : undefined);
}

/** One prepared module alone (a test that does not read the log). */
export function installModule(rig: Rig, id: string, artifact: string): Promise<void> {
  return installModules(rig, [{ id, artifact }]);
}

/** The debug module and the modules `ids`, in one selection and one restart. */
export function installWithDebug(rig: Rig, ...ids: string[]): Promise<void> {
  const device = rig.session.options.device;
  return installModules(
    rig,
    [...new Set([DEBUG_MODULE, ...ids])].map((id) => ({ id, artifact: moduleArtifact(device, id) })),
  );
}
