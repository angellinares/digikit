// The desktop Samples backend: the samples API of apps/web/src/modules over the selected target (target.ts), with the backend the device profile
// names (`[storage] samples`, read by the native `device_storage`; the page names no device). The converter is `dksample.wasm`, loaded once through
// the native `sample_wasm`. The local side is the native `local_*` commands (local.rs): pick a folder, list a level, read a file.
// Nothing is started and nothing is built here.
import { invoke } from '@tauri-apps/api/core';
import { errorLine } from '@web/modules/backend';
import { fmtVersion, type DkSession } from '@digi/dk-session';
import { Sample, prepareUpload, sourceKind, type Prepared } from '@digi/dk-session/dksample';
import { openSamples, type SamplesBackend } from '@digi/dk-session/samples';
import { BulkTakeOver, chooseBulk, type BulkChoice, type TakeOverDevice } from '@digi/dk-session/bulk-select';
import { transferEnv } from '@web/transfer/env';
import { asked, noteConnection } from '@web/transfer/connection-state';
import { portLost, resetSession, session as running, target } from '@web/transfer/target';
import { joinPath, type LocalEntry, type Prepare } from './samples-core';
import { setDeviceInfo, setSampleLink } from '@web/transfer/transfer-state';
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
  | {
      s: DkSession;
      name: string;
      runtime: string;
      samples: string;
      opened?: OpenedSamples;
      bulk?: BulkChoice;
      /** The take-over of a device in USB AUDIO/MIDI (made at the first run that needs it). */
      takeover?: BulkTakeOver;
      /** The current run called `takeover.begin`: `endRun` calls `end`. */
      began?: boolean;
    }
  | undefined;

/**
 * The device across a take-over: the MIDI port goes away when the device switches to USB MIDI and returns, so the session is read again each time (`running`
 * opens the port) and `reenumerated` waits until the device answers on it. [O] whether the port keeps its name on every host.
 */
function takeOverDevice(): TakeOverDevice {
  const port = () => {
    const t = target();
    return t.kind === 'midi' ? t.port : undefined;
  };
  const via =
    <T>(f: (s: DkSession) => Promise<T>) =>
    async (): Promise<T> =>
      f(await running());
  return {
    sysex: () => ({
      usbFastNonce: async (n) => (await running()).usbFastNonce(n),
      usbFastWrongMode: via((s) => s.usbFastWrongMode()),
      usbFastTakeOver: via((s) => s.usbFastTakeOver()),
      usbFastRelease: async () => {
        await (await running()).usbFastRelease();
        // the device switches back about 50 ms after the reply and the port goes away again: the next request opens it fresh
        const p = port();
        setTimeout(() => p && portLost(p), 300);
      },
    }),
    async reenumerated(timeoutMs) {
      const deadline = Date.now() + timeoutMs;
      await new Promise((r) => setTimeout(r, 1500));
      for (;;) {
        const p = port();
        if (p) portLost(p);
        resetSession();
        try {
          await (await running()).info();
          return;
        } catch (e) {
          if (Date.now() > deadline) throw e;
          await new Promise((r) => setTimeout(r, 500));
        }
      }
    },
  };
}

const note = (why: string) => console.info(`[samples] ${why}`);

/**
 * The samples backend of the device on the target. The session part (INFO, the storage) is kept per session. The bulk choice is made again whenever there
 * is no bulk link or the last one failed during a run (it is not cached as broken for the session), so an interface that appears later is used at the next
 * run; with no candidate on the bus that costs one USB listing. SysEx stays the channel for LIST, MKDIR, DELETE and the rest.
 */
export async function openDeviceSamples(forRun = false): Promise<OpenedSamples> {
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
  // only a run chooses again after a failure: a read between the failure and the next run keeps the fallback pill
  if (forRun && had && last.opened?.backend.bulkState === 'fallback') {
    await Promise.resolve(had.link.close()).catch(() => {});
    last.bulk = undefined;
  }
  if (!last.bulk) {
    const env = transferEnv().bulk;
    last.bulk = env && target().kind !== 'none' ? await chooseBulk(s, env, note) : undefined;
    // a device in USB AUDIO/MIDI has no bulk interface: a run takes USB over (no question; USB audio pauses until the linger after the run ends)
    if (forRun && env && !last.bulk && target().kind === 'midi' && (await s.usbFastWrongMode())) {
      last.takeover = new BulkTakeOver(takeOverDevice(), env, { note });
    }
  }
  if (forRun && last.takeover && !last.began) {
    // `begin` returns the held link at once when the device is still ours, and takes over again when the device gave the mode back
    last.began = true;
    const c = await last.takeover.begin();
    if (!c) last.began = false;
    last.bulk = c;
    if (last.opened && !had === !last.bulk && (!had || had === last.bulk))
      setSampleLink({ state: last.opened.link, takenOver: !!c });
  }
  if (last.opened && !had === !last.bulk && (!had || had === last.bulk)) {
    // a new choice for the run (the last one had failed and the link is the same again): the pill follows it
    if (forRun) setSampleLink({ state: last.opened.link });
    return last.opened;
  }
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
      onFallback: (reason) => setSampleLink({ state: 'fallback', reason }),
    }),
    prepare: (name, bytes) => prepareUpload(conv, name, bytes),
    link: last.bulk ? 'bulk' : 'sysex',
  };
  setSampleLink({ state: last.opened.link, takenOver: last.opened.link === 'bulk' && !!last.takeover?.active });
  return last.opened;
}

/** A run ended (also a failed one): the take-over, if any, keeps the device for its linger and then gives the USB mode back. */
export function endRun(): void {
  if (last?.takeover && last.began) {
    last.began = false;
    last.takeover.end();
  }
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
