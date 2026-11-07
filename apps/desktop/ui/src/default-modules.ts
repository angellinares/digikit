// The desktop side of the default modules (apps/web/src/firmware/default-modules-core.ts): the install over the protocol session of the flashed
// device or of the emulator, with the built .dkm of the library folder (`out/modules/<device>`). The Transfer pane's calls do the upload and the
// restart; the list is in default-modules-data.ts.
import type { DkSession } from '@digi/dk-session';
import { installDefaults, type DefaultOps, type DefaultProgress } from '../../../web/src/firmware/default-modules-core';
import type { FirmwareEnv } from '../../../web/src/firmware/env';
import { install, restartDevice, type LibraryModule } from '../../../web/src/modules/dktransfer';
import { asked } from '../../../web/src/transfer/connection-state';
import { emulatorSession, resetSession, session } from '../../../web/src/transfer/target';
import { loadLibrary } from '../../../web/src/transfer/transfer-backend';
import { entries, waitForRuntime } from './default-modules-data';

/** The operations of the install run over a session and the library of built modules. */
export function sessionOps(s: DkSession, library: LibraryModule[]): DefaultOps {
  const by = new Map(library.map((m) => [m.id, m]));
  return {
    pathOf: (id) => by.get(id)?.path,
    waitForRuntime: () => waitForRuntime(s),
    async upload(id) {
      const r = await install(s, [id], library);
      return r.uploaded.includes(id) ? 'uploaded' : 'same';
    },
    loaded: async () => (await s.modules()).map((m) => m.name),
    select: (paths) => s.select(paths),
    async restart() {
      return (await restartDevice(s)).loaded;
    },
  };
}

export const desktopDefaultModules: NonNullable<FirmwareEnv['defaultModules']> = {
  entries,
  async install(where, device, ids, progress: (p: DefaultProgress) => void) {
    // after a flash the old session may be of the port before the restart
    if (where === 'device') resetSession();
    const s = await (where === 'emulator' ? emulatorSession() : session());
    const library = await loadLibrary(device);
    return asked(() => installDefaults(sessionOps(s, library), ids, progress));
  },
};

/**
 * Whether the first runtime status after launch starts the default-modules install. A launch with the runtime ticked has the firmware loaded
 * by the app's Rust side, so `boot()` never runs; `started` stops a second install when an early load already began one.
 */
export function shouldInstallAtStart(s: {
  runtimeOn: boolean;
  runtimeSet: boolean;
  started: boolean;
  hasStatus: boolean;
}): boolean {
  return s.runtimeOn && s.runtimeSet && !s.started && s.hasStatus;
}
