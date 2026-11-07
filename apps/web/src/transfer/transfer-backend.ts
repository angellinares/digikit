// The Transfer backend: the protocol client and the Transfer logic of apps/web/src/modules (TypeScript), over the selected target (target.ts).
// What differs between the apps comes from the environment (env.ts): the library of built modules, patchkit.wasm and the transports.
// Nothing is built here and no process is started.
import { DkNeeds } from '../modules/dkneeds';
import { setFitReader } from '../modules/fit';
import { fileHashHex, fmtVersion, storePath } from '@digi/dk-session';
import {
  buildView,
  deleteFiles,
  install,
  libraryModule,
  restartDevice,
  type LibraryModule,
  type TransferEvent,
  type TransferView,
} from '../modules/dktransfer';
import { RPC_SOFTWARE_VERSION, parseSoftwareVersion } from './connection-core';
import { asked, noteConnection } from './connection-state';
import { transferEnv } from './env';
import { session as running, target, targetLabel } from './target';

let needsReader: Promise<DkNeeds> | undefined;
/** What a .dkm takes of the device and whether it fits: patchkit.wasm's `pk_needs` and `pk_fit`, loaded once (a failed load is tried again at the next use). */
function needs(): Promise<DkNeeds> {
  needsReader ??= transferEnv()
    .patchkitWasm()
    .then((b) => DkNeeds.create(b))
    .then((r) => {
      setFitReader(r); // the fit rule of the rows comes from the same wasm
      return r;
    })
    .catch((e) => {
      needsReader = undefined;
      throw e;
    });
  return needsReader;
}

/** The built modules of the device's folder, read again each time (a rebuild in a terminal shows up at the next read). */
export async function loadLibrary(device: string): Promise<LibraryModule[]> {
  const reader = await needs();
  const { library } = transferEnv();
  const out: LibraryModule[] = [];
  for (const f of await library.scan(device)) {
    const m = await libraryModule(await library.read(device, f.path), (b) => reader.of(b));
    if (m) out.push(m);
  }
  return out;
}

/** The OS version per target, asked once per page (also when it fails: the stock firmware answers SoftwareVersion, a device that does not leaves it out). */
const osVersions = new Map<string, Promise<string | undefined>>();
function osVersion(s: {
  rpc(type: number, payload?: Uint8Array, timeoutMs?: number): Promise<Uint8Array>;
}): Promise<string | undefined> {
  const key = targetLabel(target());
  let known = osVersions.get(key);
  if (!known) {
    known = s.rpc(RPC_SOFTWARE_VERSION, undefined, 1500).then(parseSoftwareVersion, () => undefined);
    osVersions.set(key, known);
  }
  return known;
}

/** The session, and its INFO: the answer is what the connection indicator shows. */
async function opened() {
  const s = await asked(running);
  const info = await asked(() => s.info());
  const os = await osVersion(s);
  noteConnection({
    type: 'answered',
    device: {
      id: info.port,
      name: await transferEnv().deviceName(info.port),
      ...(os ? { os } : {}),
      runtime: fmtVersion(info.runtime),
    },
  });
  return { s, info };
}

export const transferBackend = {
  /** The merged list: the library folder and what the device holds (INFO, LIST, MODULES, CAPACITY, READ). */
  async list(): Promise<TransferView> {
    const { s, info } = await opened();
    const library = await loadLibrary(info.port);
    return asked(() => buildView(s, library));
  },
  /** Upload and VERIFY the modules (the same file hash is not sent again). The device's selection is not touched. */
  async install(modules: string[], onEvent: (e: TransferEvent) => void): Promise<void> {
    const { s, info } = await opened();
    const library = await loadLibrary(info.port);
    await asked(() => install(s, modules, library, onEvent));
  },
  /** DELETE the files from the card (one request each, confirmed with LIST). The device's selection is not touched. */
  async remove(paths: string[], onEvent: (e: TransferEvent) => void): Promise<void> {
    const s = await asked(running);
    await asked(() => deleteFiles(s, paths, onEvent));
  },
  /** RESTART the emulated device, wait for the boot and report what loaded. */
  async restart(onEvent: (e: TransferEvent) => void): Promise<void> {
    const s = await asked(running);
    await asked(() => restartDevice(s, onEvent));
  },
  /** Put .dkm files from outside the library on the card (the same file hash is not sent again); returns what was done, one line each. */
  async add(files: { name: string; data: Uint8Array }[]): Promise<string[]> {
    const s = await asked(running);
    return asked(async () => {
      const onCard = new Map((await s.list()).map((e) => [e.path, e.hash]));
      const lines: string[] = [];
      for (const f of files) {
        const path = storePath(f.name);
        if (onCard.get(path) === fileHashHex(f.data)) {
          lines.push(`${path}: already on the card`);
          continue;
        }
        await s.put(path, f.data);
        lines.push(`Added ${path}`);
      }
      return lines;
    });
  },
};
