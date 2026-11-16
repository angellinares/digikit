// The Modules backend of the preview (replaces packages/workbench-core/src/transfer/transfer-backend.ts): the library and the device come from sim.ts; an install, a delete
// and a restart emit the same events as dktransfer.ts, paced by `sim.speed`.
import {
  moduleFilePath,
  moduleFilesFolder,
  type TransferEvent,
  type TransferView,
} from '@digi/workbench-core/modules/dktransfer.ts';
import { noteActivity } from '@digi/workbench-core/transfer/activity.ts';
import { PAUSED } from '../src/desktop-env';
import { LIBRARY, delay, deviceFiles, deviceRead, libraryById, pathOf, sim, simApp, view, control } from './sim';

export { PAUSED };

/** The simulated device's module files, by store path. */
const moduleFiles = new Map<string, Uint8Array>();

/** Send `bytes` at the simulated speed, with a progress event about every 100 ms. */
async function pace(bytes: number, emit: (sent: number) => void): Promise<void> {
  const step = Math.max(1, Math.round(sim.speed * 1024 * 1024 * 0.1));
  for (let sent = 0; sent < bytes;) {
    await delay(100);
    sent = Math.min(bytes, sent + Math.round(step * (0.85 + 0.3 * Math.abs(Math.sin(sent / step)))));
    noteActivity();
    emit(sent);
  }
}

export const transferBackend = {
  async list(device: string, showLibrary: (view: TransferView) => void): Promise<TransferView> {
    showLibrary(await this.listLibrary(device));
    control.counts.deviceRead++;
    const { asked, noteConnection } = simApp.app!.connection;
    await asked(deviceRead);
    noteConnection({ type: 'answered', device: { id: 'dt2', name: 'Digitakt II', runtime: '0.5.2' } });
    return view();
  },
  async listLibrary(_device: string): Promise<TransferView> {
    const v = view();
    return {
      ...v,
      runtime: '',
      free_bytes: 0,
      loaded: [],
      capacity: null,
      card_only: [],
      modules: v.modules
        .filter((m) => LIBRARY.some((l) => l.id === m.id))
        .map((m) => ({ ...m, on_card: 'no', loaded: false, warnings: [] })),
    };
  },
  async install(modules: string[], onEvent: (e: TransferEvent) => void): Promise<void> {
    await deviceRead();
    onEvent({ event: 'start', modules, device: 'dt2' });
    for (const id of modules) {
      const m = libraryById(id) ?? LIBRARY[0];
      const path = pathOf(m.id);
      // the services of the ticked module that are not on the card go first (sim.ts gives pluck two)
      const row = view().modules.find((r) => r.id === m.id);
      for (const s of row?.services?.filter((x) => x.state !== 'on device') ?? []) {
        const sid = s.stem;
        const spath = `services/${s.stem}.dks`;
        const size = Math.round(s.size);
        onEvent({ event: 'upload', kind: 'service', id: sid, path: spath, bytes: size });
        await pace(size, (sent) =>
          onEvent({ event: 'progress', kind: 'service', id: sid, path: spath, sent, bytes: size }),
        );
        onEvent({ event: 'verify', kind: 'service', id: sid, path: spath, ok: true });
      }
      onEvent({ event: 'build', id });
      await delay(150);
      onEvent({ event: 'upload', id, path, bytes: m.size });
      await pace(m.size, (sent) => onEvent({ event: 'progress', id, path, sent, bytes: m.size }));
      deviceFiles.set(path, { path, size: m.size, same: true, loaded: deviceFiles.get(path)?.loaded ?? false });
      onEvent({ event: 'verify', id, path, ok: true });
    }
    onEvent({ event: 'done', restarted: false });
  },
  async remove(paths: string[], onEvent: (e: TransferEvent) => void): Promise<void> {
    for (const path of paths) {
      onEvent({ event: 'delete', path });
      await delay(250);
      deviceFiles.delete(path);
    }
    onEvent({ event: 'done', deleted: paths });
  },
  async listModuleFiles(moduleId: string): Promise<{ name: string; size: number }[]> {
    await deviceRead();
    const folder = moduleFilesFolder(moduleId);
    return [...moduleFiles.entries()]
      .filter(([path]) => path.startsWith(folder))
      .map(([path, bytes]) => ({ name: path.slice(folder.length), size: bytes.length }));
  },
  async putModuleFile(
    moduleId: string,
    name: string,
    bytes: Uint8Array,
    progress: (sent: number, total: number) => void,
  ): Promise<void> {
    const path = moduleFilePath(moduleId, name);
    await pace(bytes.length, (sent) => progress(sent, bytes.length));
    moduleFiles.set(path, bytes);
  },
  async readModuleFile(moduleId: string, name: string): Promise<Uint8Array> {
    const bytes = moduleFiles.get(moduleFilePath(moduleId, name));
    if (!bytes) throw new Error(`${name} is not on the device`);
    return bytes;
  },
  async removeModuleFile(moduleId: string, name: string): Promise<void> {
    await delay(250);
    moduleFiles.delete(moduleFilePath(moduleId, name));
  },
  async restart(onEvent: (e: TransferEvent) => void): Promise<void> {
    onEvent({ event: 'restart' });
    await delay(1200);
    onEvent({ event: 'done', loaded: [...deviceFiles.values()].filter((f) => f.loaded).map((f) => f.path) });
  },
  async refreshServices(): Promise<string[]> {
    return [];
  },
  async add(files: { name: string; data: Uint8Array }[]): Promise<string[]> {
    await delay(300);
    return files.map((f) => `Added modules/${f.name}`);
  },
};

/** The same bag as the real `createTransferBackend` (packages/workbench-core/src/transfer/transfer-backend.ts); the preview reads the library from sim.ts. */
export const createTransferBackend = (_deps: unknown) => ({
  transferBackend,
  loadLibrary: async () => [],
  installDeps: async () => undefined,
});
