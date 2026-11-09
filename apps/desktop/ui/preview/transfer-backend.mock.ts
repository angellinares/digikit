// The Modules backend of the preview (replaces ../src/transfer-backend.ts): the library and the device come from sim.ts; an install, a delete
// and a restart emit the same events as dktransfer.ts, paced by `sim.speed`.
import { moduleFilePath, moduleFilesFolder, type TransferEvent, type TransferView } from '@web/modules/dktransfer';
import { noteActivity } from '@web/transfer/activity';
import { asked, noteConnection } from '@web/transfer/connection-state';
import { PAUSED } from '../src/desktop-env';
import { LIBRARY, delay, deviceFiles, deviceRead, libraryById, pathOf, sim, view, control } from './sim';

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
  async list(): Promise<TransferView> {
    control.counts.deviceRead++;
    await asked(deviceRead);
    noteConnection({ type: 'answered', device: { id: 'dt2', name: 'Digitakt II', runtime: '0.5.2' } });
    return view();
  },
  async install(modules: string[], onEvent: (e: TransferEvent) => void): Promise<void> {
    await deviceRead();
    onEvent({ event: 'start', modules, device: 'dt2' });
    for (const id of modules) {
      const m = libraryById(id) ?? LIBRARY[0];
      const path = pathOf(m.id);
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
  async add(files: { name: string; data: Uint8Array }[]): Promise<string[]> {
    await delay(300);
    return files.map((f) => `Added modules/${f.name}`);
  },
};
