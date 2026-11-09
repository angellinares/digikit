// The Samples backend of the preview (replaces ../src/samples-backend.ts): the folder on the computer and the device's sample folders come
// from sim.ts. Same exports as the real module; an upload is paced by `sim.speed` through the real `uploadFiles` of samples-core.ts.
import { noteActivity } from '@web/transfer/activity';
import { asked, noteConnection } from '@web/transfer/connection-state';
import { sourceKind, type Prepared } from '@digi/dk-session/dksample';
import type { SampleEntry, SamplesBackend } from '@digi/dk-session/samples';
import { joinPath, type LocalEntry, type Prepare } from '../src/samples-core';
import { setDeviceInfo } from '@web/transfer/transfer-state';
import {
  localFolder,
  localPath,
  sampleFolder,
  setLocalEntries,
  setLocalKinds,
  setSampleEntries,
} from '../src/samples-state';
import { SAMPLES_ROOT, control, delay, deviceRead, listLocal, localKindOf, localSizeOf, sampleTree, sim } from './sim';

const NAME = 'Digitakt II';
export function deviceStorage(_device: string): Promise<{ name: string; samples: string }> {
  return Promise.resolve({ name: NAME, samples: 'stock-rpc' });
}

export interface OpenedSamples {
  name: string;
  runtime: string;
  samples: string;
  backend: SamplesBackend;
  prepare: Prepare;
}

let puts = 0;
const clean = (folder: string) => folder.replace(/^\/+|\/+$/g, '');
const backend: SamplesBackend = {
  kind: 'stock-rpc',
  async list(folder = ''): Promise<SampleEntry[]> {
    await deviceRead();
    return [...(sampleTree.get(clean(folder)) ?? [])];
  },
  async space() {
    return { free: 3.1 * 2 ** 30, total: 4 * 2 ** 30 };
  },
  async canMkdir() {
    return sim.mkdir;
  },
  async mkdir(folder: string) {
    await delay(200);
    const f = clean(folder);
    const parent = f.split('/').slice(0, -1).join('/');
    sampleTree.set(f, []);
    sampleTree.set(parent, [
      ...(sampleTree.get(parent) ?? []),
      { name: f.split('/').pop() as string, kind: 'folder', size: 0 },
    ]);
  },
  async rmdir(folder: string) {
    const f = clean(folder);
    sampleTree.delete(f);
    const parent = f.split('/').slice(0, -1).join('/');
    sampleTree.set(
      parent,
      (sampleTree.get(parent) ?? []).filter((e) => e.name !== f.split('/').pop()),
    );
  },
  async removeTree(folder, o) {
    const root = clean(folder);
    const files: [string, string][] = [];
    const dirs: string[] = [];
    const walk = (dir: string) => {
      for (const e of sampleTree.get(dir) ?? []) {
        if (e.kind === 'folder') walk(`${dir}/${e.name}`);
        else files.push([dir, e.name]);
      }
      dirs.push(dir);
    };
    walk(root);
    o?.onProgress?.(0, files.length);
    let removed = 0;
    for (const [dir, name] of files) {
      if (o?.shouldStop?.()) return { removed, total: files.length, stopped: true };
      await delay(150);
      sampleTree.set(
        dir,
        (sampleTree.get(dir) ?? []).filter((e) => e.name !== name),
      );
      removed++;
      o?.onProgress?.(removed, files.length);
    }
    for (const d of dirs) await backend.rmdir(d);
    return { removed, total: files.length, stopped: false };
  },
  sameOnDevice(file) {
    return sim.skip.split(',').includes(file.name);
  },
  async put(folder, file, onProgress) {
    if (sim.putfail && puts++ === 1) throw new Error(`open /tst/${file.name}: the device answered status 0`);
    const total = file.stored.length;
    const step = Math.max(1, Math.round(sim.speed * 1024 * 1024 * 0.1));
    for (let sent = 0; sent < total;) {
      await delay(100);
      sent = Math.min(total, sent + Math.round(step * (0.85 + 0.3 * Math.abs(Math.sin(sent / step)))));
      noteActivity();
      onProgress?.(sent, total);
    }
    const f = clean(folder);
    const list = (sampleTree.get(f) ?? []).filter((e) => e.name !== file.name);
    sampleTree.set(f, [...list, { name: file.name, kind: 'file', size: total }]);
  },
  async verify(_folder, file) {
    await delay(200);
    const size = file.stored.length;
    return {
      ok: true,
      device: 'preview',
      host: 'preview',
      size,
      sent: size,
      mismatchAt: null,
      compared: 'device content hash matches',
      message: `${size} bytes compared`,
    };
  },

  async remove(folder, name) {
    await delay(150);
    const f = clean(folder);
    sampleTree.set(
      f,
      (sampleTree.get(f) ?? []).filter((e) => e.name !== name),
    );
  },
};

export async function openDeviceSamples(): Promise<OpenedSamples> {
  await asked(deviceRead);
  noteConnection({ type: 'answered', device: { id: 'dt2', name: NAME, runtime: '0.5.2' } });
  return {
    name: NAME,
    runtime: '0.9.4',
    samples: 'stock-rpc',
    backend,
    prepare: (name, bytes): Prepared => {
      const kind = sourceKind(bytes, bytes.length);
      return { name: kind === 'converted' ? name.replace(/\.wav$/i, '') : name, bytes, stored: bytes, kind };
    },
  };
}

export async function readSamples(): Promise<void> {
  control.counts.deviceRead++;
  const { name, runtime, samples } = await openDeviceSamples();
  setSampleEntries(await backend.list(sampleFolder()));
  setDeviceInfo({ name, runtime, samples, canMkdir: sim.mkdir, free: (await backend.space()).free });
}

// ---- the local folder -------------------------------------------------------------------------------------------------------------

export const pickFolder = async (_start: string): Promise<string | null> => SAMPLES_ROOT;

/** The bytes of a file of the folder: zeros of the right size, with a WAV head on a .wav (`head`: only the first bytes). */
export async function readLocalFile(rel: string, head?: number): Promise<Uint8Array> {
  const size = head ?? localSizeOf(rel);
  const out = new Uint8Array(Math.min(size, head ?? size));
  if (rel.toLowerCase().endsWith('.wav')) out.set(new TextEncoder().encode('RIFF'), 0);
  if (rel.toLowerCase().endsWith('.wav')) out.set(new TextEncoder().encode('WAVE'), 8);
  return out;
}

export async function labelOf(read: () => Promise<Uint8Array>, size: number): Promise<Prepared['kind'] | undefined> {
  try {
    return sourceKind(await read(), size);
  } catch {
    return undefined;
  }
}

/** One level of the folder at `rel` (what an upload of a folder walks). */
export async function listLocalAt(rel: string): Promise<LocalEntry[]> {
  await delay(20);
  return listLocal(rel);
}

/** The open level as it is now (the listing only); counted, so a test can see how often the poll asks. */
export async function listLocalNow(): Promise<LocalEntry[] | undefined> {
  if (!localFolder() || sim.folder === 'none') return undefined;
  control.counts.localPoll++;
  return listLocal(localPath());
}

/** Read the open level of the chosen folder and label its files; the same effects as the real `readLocal`. */
export async function readLocal(): Promise<void> {
  if (!localFolder() || sim.folder === 'none') {
    setLocalEntries(undefined);
    return;
  }
  control.counts.localList++;
  await delay(60);
  const path = localPath();
  const entries: LocalEntry[] = listLocal(path);
  setLocalEntries(entries);
  const kinds: Record<string, Prepared['kind']> = {};
  for (const e of entries)
    if (e.kind === 'file') {
      const rel = joinPath(path, e.name);
      const k = localKindOf(rel);
      if (k) kinds[rel] = k;
    }
  setLocalKinds((k) => ({ ...Object.fromEntries(Object.entries(k).filter(([key]) => key.startsWith('+'))), ...kinds }));
}
