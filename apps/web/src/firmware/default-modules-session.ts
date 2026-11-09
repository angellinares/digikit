// The install of the default modules over a protocol session (default-modules-core.ts), shared by the desktop app and the site: the session of
// the flashed device or of the emulator, with the built .dkm of the library folder.
import type { DkSession } from '@digi/dk-session';
import { install, restartDevice, type LibraryModule } from '../modules/dktransfer';
import type { DefaultOps } from './default-modules-core';
import { waitForRuntime } from './default-modules-data';

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
