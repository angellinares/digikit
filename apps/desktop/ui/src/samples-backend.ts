// The desktop Samples backend: the samples API of apps/web/src/modules over the selected target (target.ts), with the backend the device profile
// names (`[storage] samples`, read by the native `device_storage`; the page names no device). The converter is `dksample.wasm`, loaded once through
// the native `sample_wasm`. The local side is the native `local_*` commands (local.rs): pick a folder, list a level, read a file.
// Nothing is started and nothing is built here.
import { invoke } from '@tauri-apps/api/core';
import { errorLine } from '@web/modules/backend';
import { fmtVersion, type DkSession } from '@digi/dk-session';
import { Sample, prepareUpload, sourceKind, type Prepared } from '@digi/dk-session/dksample';
import { openSamples, type SamplesBackend } from '@digi/dk-session/samples';
import { chooseBulk, type BulkChoice } from '@digi/dk-session/bulk-select';
import { transferEnv } from '@web/transfer/env';
import { asked, noteConnection } from '@web/transfer/connection-state';
import { session as running, target } from '@web/transfer/target';
import { joinPath, type LocalEntry, type Prepare } from './samples-core';
import { setDeviceInfo } from '@web/transfer/transfer-state';
import {
  localFolder,
  localPath,
  sampleFolder,
  setLocalEntries,
  setLocalKinds,
  setSampleEntries,
} from './samples-state';

/** What the device profile says about the device with this id (`dt2`): its name and the backend of its samples. */
const storages = new Map<string, Promise<{ name: string; samples: string }>>();
export function deviceStorage(device: string): Promise<{ name: string; samples: string }> {
  let p = storages.get(device);
  if (!p) {
    p = invoke<{ name: string; samples: string }>('device_storage', { device });
    p.catch(() => storages.delete(device));
    storages.set(device, p);
  }
  return p;
}

/** The device behind the target: its name, its runtime, the backend of its samples, and the open backend. */
export interface OpenedSamples {
  name: string;
  runtime: string;
  samples: string;
  backend: SamplesBackend;
  prepare: Prepare;
  /** Where the file bytes go: the USB bulk link (usb-fast) or the SysEx / MidiRpc backend. */
  link: 'bulk' | 'sysex';
}

let sample: Promise<Sample> | undefined;
/** The converter, made once (a failed load is tried again at the next use). */
function converter(): Promise<Sample> {
  sample ??= invoke<ArrayBuffer>('sample_wasm')
    .then((b) => Sample.create(b))
    .catch((e) => {
      sample = undefined;
      throw e;
    });
  return sample;
}

let last:
  | { s: DkSession; name: string; runtime: string; samples: string; opened?: OpenedSamples; bulk?: BulkChoice }
  | undefined;

const note = (why: string) => console.info(`[samples] ${why}`);

/**
 * The samples backend of the device on the target. The session part (INFO, the storage) is kept per session. The bulk choice is made again whenever there
 * is no bulk link or the last one failed during a run (it is not cached as broken for the session), so an interface that appears later is used at the next
 * run; with no candidate on the bus that costs one USB listing. SysEx stays the channel for LIST, MKDIR, DELETE and the rest.
 */
export async function openDeviceSamples(): Promise<OpenedSamples> {
  const s = await asked(running);
  if (last && last.s !== s) {
    // another session: let go of the bulk link of the old one (the interface stays claimed until then)
    await Promise.resolve(last.bulk?.link.close()).catch(() => {});
    last = undefined;
  }
  if (!last) {
    const info = await asked(() => s.info());
    const storage = await deviceStorage(info.port);
    noteConnection({
      type: 'answered',
      device: { id: info.port, name: storage.name, runtime: fmtVersion(info.runtime) },
    });
    last = { s, name: storage.name, runtime: fmtVersion(info.runtime), samples: storage.samples };
  }
  const had = last.bulk;
  if (had && last.opened?.backend.bulkState === 'fallback') {
    await Promise.resolve(had.link.close()).catch(() => {});
    last.bulk = undefined;
  }
  if (!last.bulk) {
    const env = transferEnv().bulk;
    last.bulk = env && target().kind === 'midi' ? await chooseBulk(s, env, note) : undefined;
  }
  if (last.opened && !had === !last.bulk && (!had || had === last.bulk)) return last.opened;
  const conv = await converter();
  last.opened = {
    name: last.name,
    runtime: last.runtime,
    samples: last.samples,
    backend: openSamples(last.samples, {
      session: s,
      hashOf: (d) => conv.hash(d),
      bulk: last.bulk?.session,
      note,
    }),
    prepare: (name, bytes) => prepareUpload(conv, name, bytes),
    link: last.bulk ? 'bulk' : 'sysex',
  };
  return last.opened;
}

/** Read the device's open folder, its free space and what the backend can do (the On device column of Samples mode). */
export async function readSamples(): Promise<void> {
  const { name, runtime, samples, backend } = await openDeviceSamples();
  const canMkdir = await backend.canMkdir();
  setSampleEntries(await backend.list(sampleFolder()));
  let free: number | undefined;
  try {
    free = (await backend.space()).free;
  } catch {
    // the listing is what counts; the free space is a line
  }
  setDeviceInfo({ name, runtime, samples, canMkdir, free });
}

// ---- the local folder ---------------------------------------------------------------------------------------------------------------

/** The system's folder dialog; null when the user cancels. */
export const pickFolder = (start: string): Promise<string | null> =>
  invoke<string | null>('local_pick_folder', { start: start || null });

/** The bytes of a file of the chosen folder (`head`: only the first bytes). */
export async function readLocalFile(rel: string, head?: number): Promise<Uint8Array> {
  return new Uint8Array(await invoke<ArrayBuffer>('local_read', { root: localFolder(), rel, head: head ?? null }));
}

/** How many files are labelled per read of the folder (the first bytes of each are read; a larger folder keeps the rest unlabelled). */
const LABELLED = 400;

/** What the upload will do with a file, from its first 64 bytes; undefined when they cannot be read. */
export async function labelOf(read: () => Promise<Uint8Array>, size: number): Promise<Prepared['kind'] | undefined> {
  try {
    return sourceKind(await read(), size);
  } catch {
    return undefined;
  }
}

/** One level of the chosen folder at `rel` (the listing only; an upload of a folder walks the folders with it). */
export const listLocalAt = (rel: string): Promise<LocalEntry[]> =>
  invoke<LocalEntry[]>('local_list', { root: localFolder(), rel });

/** The open level of the chosen folder as it is now (the listing only, no file is read): the light poll compares it with what the page shows.
 * Undefined when no folder is chosen or it cannot be listed. */
export async function listLocalNow(): Promise<LocalEntry[] | undefined> {
  if (!localFolder()) return undefined;
  try {
    return await invoke<LocalEntry[]>('local_list', { root: localFolder(), rel: localPath() });
  } catch {
    return undefined;
  }
}

/** Read the open level of the chosen folder and label its files. No folder chosen: an empty list. */
export async function readLocal(): Promise<void> {
  if (!localFolder()) {
    setLocalEntries(undefined);
    return;
  }
  const path = localPath();
  let entries: LocalEntry[];
  try {
    entries = await invoke<LocalEntry[]>('local_list', { root: localFolder(), rel: path });
  } catch (e) {
    setLocalEntries([]);
    throw new Error(errorLine(e));
  }
  setLocalEntries(entries);
  const files = entries.filter((e) => e.kind === 'file').slice(0, LABELLED);
  const kinds: Record<string, Prepared['kind']> = {};
  for (let i = 0; i < files.length; i += 16) {
    await Promise.all(
      files.slice(i, i + 16).map(async (f) => {
        const rel = joinPath(path, f.name);
        const kind = await labelOf(() => readLocalFile(rel, 64), f.size);
        if (kind) kinds[rel] = kind;
      }),
    );
  }
  setLocalKinds((k) => ({ ...Object.fromEntries(Object.entries(k).filter(([key]) => key.startsWith('+'))), ...kinds }));
}
