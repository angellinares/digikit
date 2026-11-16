// The desktop side of the default modules (packages/workbench-core/src/firmware/default-modules-core.ts): the install over the protocol session of the flashed
// device or of the emulator, with the built .dkm of the library folder (`out/modules/<device>`). The Transfer pane's calls do the upload and the
// restart; the list is in default-modules-data.ts.
import type { DkSession } from '@digi/dk-session';
import {
  installDefaults,
  type DefaultOps,
  type DefaultProgress,
} from '@digi/workbench-core/firmware/default-modules-core.ts';
import type { FirmwareEnv } from '@digi/workbench-core/firmware/env.ts';
import {
  install,
  restartDevice,
  type InstallDeps,
  type LibraryModule,
} from '@digi/workbench-core/modules/dktransfer.ts';
import type { TransferApp } from '@digi/workbench-core/transfer/app.ts';
import { entries, waitForRuntime } from './default-modules-data';

/** The operations of the install run over a session and the library of built modules; with `deps` each module comes with the services it needs. */
export function sessionOps(s: DkSession, library: LibraryModule[], deps?: InstallDeps): DefaultOps {
  const by = new Map(library.map((m) => [m.id, m]));
  return {
    pathOf: (id) => by.get(id)?.path,
    waitForRuntime: () => waitForRuntime(s),
    async upload(id) {
      const r = await install(s, [id], library, undefined, deps);
      if (r.refused?.length)
        throw new Error(
          `not installed: it needs the ${r.refused[0]!.service} service and the workbench has no matching one (${r.refused[0]!.kind})`,
        );
      return r.uploaded.includes(id) ? 'uploaded' : 'same';
    },
    loaded: async () => (await s.modules()).map((m) => m.name),
    select: (paths) => s.select(paths),
    async restart(opts) {
      return (await restartDevice(s, undefined, undefined, undefined, opts)).loaded;
    },
  };
}

/** The default modules of the desktop app over the session of the flashed device or of the emulator (`app` is the app that owns the sessions). */
export function createDesktopDefaultModules(app: () => TransferApp): NonNullable<FirmwareEnv['defaultModules']> {
  return {
    entries,
    async install(where, device, ids, progress: (p: DefaultProgress) => void, opts) {
      const { target, connection, loadLibrary, installDeps } = app();
      // after a flash the old session may be of the port before the restart
      if (where === 'device') target.resetSession();
      const s = await (where === 'emulator' ? target.emulatorSession() : target.session());
      const library = await loadLibrary(device);
      const deps = await installDeps(device);
      return connection.asked(() => installDefaults(sessionOps(s, library, deps), ids, progress, opts));
    },
  };
}
