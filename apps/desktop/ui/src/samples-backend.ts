// The desktop Samples backend: the samples API of packages/workbench-core/src/modules over the selected target (target.ts), with the backend the device profile
// names (`[storage] samples`, read by the native `device_storage`; the page names no device). The converter is `dksample.wasm`, loaded once through
// the native `sample_wasm`. The local side is the native `local_*` commands (local.rs): pick a folder, list a level, read a file.
// Nothing is started and nothing is built here.
import { errorLine } from '@digi/workbench-core/modules/backend.ts';
import { failureModule, fmtVersion, hasCap, type DkSession, type LoadFailure } from '@digi/dk-session';
import { Sample, prepareUpload, sourceKind, type Prepared } from '@digi/dk-session/dksample';
import { openSamples, type SamplesBackend } from '@digi/dk-session/samples';
import { chooseBulk, type BulkChoice } from '@digi/dk-session/bulk-select';
import type { TransferApp } from '@digi/workbench-core/transfer/app.ts';
import type { Host } from '@digi/workbench-core/host/host.ts';
import { joinPath, type LocalEntry, type Prepare } from './samples-core';
import type { LinkState } from '@digi/workbench-core/transfer/link-core.ts';
import { logEvent } from './log-state';
import {
  localFolder,
  localPath,
  sampleFolder,
  setLocalEntries,
  setLocalKinds,
  setSampleEntries,
} from './samples-state';

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

/** The load failure of the usb-fast module (the device's LOADS record), when the last boot selected it and it did not load; undefined when the runtime
 * cannot say (an older runtime) or the module loaded. */
async function usbFastFailure(s: DkSession): Promise<LoadFailure | undefined> {
  try {
    if (!hasCap(await s.info(true), 'LOADS')) return undefined;
    return (await s.loadFailures()).find((f) => failureModule(f) === 'usb-fast');
  } catch {
    return undefined;
  }
}

/** The usb-fast module does not answer the nonce: it is not loaded, so the pill stays a plain SYSEX (no reason to show). */
const NOT_LOADED = /did not take the nonce/;

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

/** The desktop Samples backend over `host` and the app's target, connection and page state. Each backend keeps its own caches (the device's storage, the converter, the session's samples backend). */
export function createSamplesBackend(host: Host, app: TransferApp) {
  const { asked, noteConnection } = app.connection;
  const { session: running, target } = app.target;
  const { setDeviceInfo, setSampleLink } = app.state;
  /** What the device profile says about the device with this id (`dt2`): its name and the backend of its samples. */
  const storages = new Map<string, Promise<{ name: string; samples: string }>>();
  function deviceStorage(device: string): Promise<{ name: string; samples: string }> {
    let p = storages.get(device);
    if (!p) {
      p = host.library.deviceStorage(device);
      p.catch(() => storages.delete(device));
      storages.set(device, p);
    }
    return p;
  }

  let sample: Promise<Sample> | undefined;
  /** The converter, made once (a failed load is tried again at the next use). */
  function converter(): Promise<Sample> {
    sample ??= host.firmware
      .sampleWasm()
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
        /** Why usb-fast did not load at the last boot, read once per session when there is no bulk link (`usbFast` unset: it did load, or it is not selected). */
        failure?: { usbFast?: LoadFailure };
        bulk?: BulkChoice;
        /** usb-fast is loaded but the device is in USB AUDIO/MIDI and no bulk route was found (firmware without core/usb-bulk-audio); read at each choice while there is no bulk link. */
        wrongMode?: boolean;
        /** The last note of the last bulk choice (why no bulk link was chosen); undefined when the choice said nothing. */
        reason?: string;
        /** The last note written to the Log drawer for this session (the choice repeats at every refresh; the same text is logged once). */
        logged?: string;
      }
    | undefined;

  /** Every line goes to the console; the Log drawer gets it unless it repeats the last line of this session. */
  const note = (why: string) => {
    console.info(`[samples] ${why}`);
    if (last && last.logged === why) return;
    if (last) last.logged = why;
    logEvent('transfer', 'info', `samples: ${why}`);
  };

  /**
   * The samples backend of the device on the target. The session part (INFO, the storage) is kept per session. The bulk choice is made again whenever there
   * is no bulk link or the last one failed during a run (it is not cached as broken for the session), so an interface that appears later is used at the next
   * run; with no candidate on the bus that costs one USB listing. SysEx stays the channel for LIST, MKDIR, DELETE and the rest.
   */
  async function openDeviceSamples(forRun = false): Promise<OpenedSamples> {
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
    // A run chooses the link fresh: the device ends an idle bulk session 21 s after the last nonce or write, and a new nonce opens a new session
    // (this also covers an unplugged interface). A read keeps the cached link, so a read between a failure and the next run keeps the fallback pill.
    if (forRun && had) {
      await Promise.resolve(had.link.close()).catch(() => {});
      last.bulk = undefined;
    }
    if (!last.bulk && !last.failure) last.failure = { usbFast: await usbFastFailure(s) };
    if (!last.bulk) {
      const env = app.env.bulk;
      let choiceNote: string | undefined;
      last.bulk =
        env && target().kind !== 'none'
          ? await chooseBulk(s, env, (line) => {
              choiceNote = line;
              note(line);
            })
          : undefined;
      // The reason is the last note of the choice. A choice with no note (no candidate on the bus) or the nonce note (usb-fast not loaded) gives none:
      // we cannot tell a missing interface from a module that is not loaded, so the pill stays a plain SYSEX.
      last.reason =
        choiceNote && !NOT_LOADED.test(choiceNote) ? choiceNote.replace(/^USB bulk link not used: /, '') : undefined;
      // chooseBulk finds the USB MIDI pair or the OUT-only interface of USB AUDIO/MIDI. A device in USB AUDIO/MIDI with neither (older firmware) stays on
      // SysEx and the pill says how to get Fast USB.
      last.wrongMode = !last.bulk && target().kind === 'midi' ? await s.usbFastWrongMode() : false;
    }
    if (last.opened && !had === !last.bulk && (!had || had === last.bulk)) {
      // a new choice for the run (the last one had failed and the link is the same again): the pill follows it
      if (forRun) setSampleLink(linkOf(last.opened.link));
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
        onFallback: (reason) => {
          logEvent('transfer', 'error', `samples: Fast USB stopped, the rest goes over SysEx: ${reason}`);
          setSampleLink({ state: 'fallback', reason });
        },
      }),
      prepare: (name, bytes) => prepareUpload(conv, name, bytes),
      link: last.bulk ? 'bulk' : 'sysex',
    };
    setSampleLink(linkOf(last.opened.link));
    return last.opened;
  }

  /** The pill state for a link: a SysEx link says why usb-fast is not in use when the device reported it. */
  function linkOf(link: 'bulk' | 'sysex'): LinkState {
    if (link === 'bulk') return { state: 'bulk' };
    const failure = last?.failure?.usbFast;
    if (failure) return { state: 'sysex', failure };
    if (last?.wrongMode) return { state: 'sysex', wrongMode: true };
    return last?.reason ? { state: 'sysex', reason: last.reason } : { state: 'sysex' };
  }

  /** Read the device's open folder, its free space and what the backend can do (the On device column of Samples mode). */
  async function readSamples(): Promise<void> {
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
  const pickFolder = (start: string): Promise<string | null> => host.fs.pickFolder({ start: start || null });

  /** The bytes of a file of the chosen folder (`head`: only the first bytes). */
  async function readLocalFile(rel: string, head?: number): Promise<Uint8Array> {
    return new Uint8Array(await host.fs.read(localFolder(), rel, head ?? null));
  }

  /** One level of the chosen folder at `rel` (the listing only; an upload of a folder walks the folders with it). */
  const listLocalAt = (rel: string): Promise<LocalEntry[]> => host.fs.list(localFolder(), rel);

  /** The open level of the chosen folder as it is now (the listing only, no file is read): the light poll compares it with what the page shows.
   * Undefined when no folder is chosen or it cannot be listed. */
  async function listLocalNow(): Promise<LocalEntry[] | undefined> {
    if (!localFolder()) return undefined;
    try {
      return await host.fs.list(localFolder(), localPath());
    } catch {
      return undefined;
    }
  }

  let localReadGeneration = 0;
  /** Read the open level of the chosen folder and label its files. No folder chosen: an empty list. */
  async function readLocal(): Promise<void> {
    const generation = ++localReadGeneration;
    const folder = localFolder();
    if (!folder) {
      setLocalEntries(undefined);
      return;
    }
    const path = localPath();
    const current = () => generation === localReadGeneration && localFolder() === folder && localPath() === path;
    let entries: LocalEntry[];
    try {
      entries = await host.fs.list(folder, path);
    } catch (e) {
      if (!current()) return;
      setLocalEntries([]);
      throw new Error(errorLine(e));
    }
    if (!current()) return;
    setLocalEntries(entries);
    const files = entries.filter((e) => e.kind === 'file').slice(0, LABELLED);
    const kinds: Record<string, Prepared['kind']> = {};
    for (let i = 0; i < files.length; i += 16) {
      if (!current()) return;
      await Promise.all(
        files.slice(i, i + 16).map(async (f) => {
          const rel = joinPath(path, f.name);
          const kind = await labelOf(() => readLocalFile(rel, 64), f.size);
          if (kind) kinds[rel] = kind;
        }),
      );
    }
    if (!current()) return;
    setLocalKinds((k) => ({
      ...Object.fromEntries(Object.entries(k).filter(([key]) => key.startsWith('+'))),
      ...kinds,
    }));
  }

  return {
    deviceStorage,
    openDeviceSamples,
    readSamples,
    pickFolder,
    readLocalFile,
    listLocalAt,
    listLocalNow,
    readLocal,
  };
}

/** What the Samples mode and the Transfer environment use. */
export type SamplesBackendApi = ReturnType<typeof createSamplesBackend>;
