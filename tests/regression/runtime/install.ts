import fs from 'node:fs';
import path from 'node:path';
import type { DkSession } from '@digi/dk-session';
import { RuntimeEvents } from '@digi/dk-session';
import { emulatorClock, repoRoot, type Device, type Session } from '@digi/test-support';
import { expect } from 'vitest';
import { type PatchedSession } from '../support.ts';
import { timePhase } from '../timing.ts';
import { boundService, neededServices, servicePath, SERVICE_MODULES } from './service-files.ts';
import { bootClockOf, pollEmulated, waitForBootEmulated } from './waits.ts';

/** Emulated milliseconds the boot-time loads of the installed modules may take after the boot is seen. */
const MODULES_LOADED_EMULATED_MS = 30_000;

/** Prepared separately with patchkit pack; acceptance never builds a toolchain, module or daemon. A service (`role = "service"`) is a `.dks`, any other module a `.dkm`. */
export function moduleArtifact(device: Device, id: string): string {
  const directory = process.env.DIGI_RUNTIME_MODULES ?? path.join(repoRoot(), 'dist/modules');
  const leaf = path.join(directory, device, id.split('/').at(-1)!);
  // a service (role 1) packs to a .dks, every other module to a .dkm
  const artifact = fs.existsSync(`${leaf}.dkm`) || !fs.existsSync(`${leaf}.dks`) ? `${leaf}.dkm` : `${leaf}.dks`;
  if (!fs.existsSync(artifact)) throw new Error(`missing ${artifact}: run nx run ${id}:pack`);
  return artifact;
}

/** The debug module: the log (LOG, PEEK, LOGCFG) exists only while it is loaded, so a test that reads the log selects it next to the module it tests. */
export const DEBUG_MODULE = 'digikit/debug';

/**
 * Native USB upload of prepared modules, one selection with all of them (in this order), one restart, then wait for their registration.
 * With `device`, the services that the modules need (`SERVICE_MODULES`: `fx.dks` for an effect) go on the card too, bound to the stock firmware (the Nx target
 * `<service id>:bind`; the runtime finds them by name at boot). With `session` and the debug module in the list, the wait ends on the log's FW_READY: the boot intro
 * is over and the stock MidiRpc service answers (without the log, the session's own probe before the first stock request covers it).
 */
export async function installModules(
  dk: DkSession,
  modules: { id: string; artifact: string }[],
  { device, session }: { device?: Device; session?: Pick<Session, 'status' | 'client'> } = {},
): Promise<void> {
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
      if (device) {
        for (const name of neededServices(modules.map((m) => m.artifact))) {
          const bound = fs.readFileSync(
            await boundService(device, name, moduleArtifact(device, SERVICE_MODULES[name]!)),
          );
          await timePhase('service.upload', () => dk.put(servicePath(name), bound));
          expect(
            await dk.verifyFile(servicePath(name), bound),
            `fresh card read matches the bound ${name} service`,
          ).toBe(true);
        }
      }
      await dk.select(paths);
      await timePhase('protocol.restart', () => dk.restart());
      await timePhase('protocol.waitForBoot', () => waitForBootEmulated(dk));
      if (session && modules.some((m) => m.id === DEBUG_MODULE)) {
        await timePhase('boot.fwReady', async () => {
          const events = new RuntimeEvents(dk, emulatorClock(session));
          await events.listen({ fromBoot: true });
          await events.untilFwReady();
        });
      }
      const want = modules.map((m) => m.id);
      const loaded = async () => (await dk.modules()).map((entry) => entry.name);
      const clock = bootClockOf(dk);
      if (clock) {
        // the module loads (DSP parts take many blocks) run in emulated time: wait for them in emulated time
        await pollEmulated(clock, loaded, (names) => want.every((id) => names.includes(id)), {
          emulatedMs: MODULES_LOADED_EMULATED_MS,
          message: `modules ${want.join(' ')} not listed`,
          retry: true,
        });
      } else {
        await expect.poll(loaded, { timeout: 30_000, interval: 500 }).toEqual(expect.arrayContaining(want));
      }
    },
    { detail: modules.map((m) => m.id).join(' ') },
  );
}

/** One prepared module alone (a test that does not read the log). Pass `device` to put the services it needs on the card. */
export function installModule(
  dk: DkSession,
  id: string,
  artifact: string,
  options?: { device?: Device; session?: Pick<Session, 'status' | 'client'> },
): Promise<void> {
  return installModules(dk, [{ id, artifact }], options);
}

/** The debug module and the modules `ids`, in one selection and one restart. */
export function installWithDebug({ session, dk }: PatchedSession, ...ids: string[]): Promise<void> {
  const device = session.options.device;
  return installModules(
    dk,
    [...new Set([DEBUG_MODULE, ...ids])].map((id) => ({ id, artifact: moduleArtifact(device, id) })),
    { device, session },
  );
}
