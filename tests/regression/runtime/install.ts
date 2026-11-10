import fs from 'node:fs';
import path from 'node:path';
import type { DkSession } from '@digi/dk-session';
import { repoRoot, type Device } from '@digi/test-support';
import { expect } from 'vitest';
import { type PatchedSession } from '../support.ts';
import { timePhase } from '../timing.ts';
import { waitForBootEmulated } from './waits.ts';

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
      await timePhase('protocol.waitForBoot', () => waitForBootEmulated(dk));
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
