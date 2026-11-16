// The Transfer backend of one app (createTransferBackend): the protocol client and the Transfer logic of packages/workbench-core/src/modules (TypeScript), over the selected target (target.ts).
// What differs between the apps comes from the environment (env.ts): the library of built modules, patchkit.wasm and the transports.
// Nothing is built here and no process is started.
import { DkNeeds, type NeedsLike } from '../modules/dkneeds';
import { setFitReader } from '../modules/fit';
import { RUNTIME_SERVICE_NAMES, fileHashHex, fmtVersion, storePath } from '@digi/dk-session';
import type { DkInfoFull, DkSession } from '@digi/dk-session';
import { NoRuntimeError, isCancelled, isTimeout } from '../modules/backend';
import {
  nameHash,
  serviceFileOf,
  type RuntimeIdentity,
  type ServiceBinder,
  type ServiceFile,
} from '../modules/services';
import {
  SERVICE_EXT,
  buildView,
  deleteFiles,
  install,
  libraryModule,
  listModuleFiles,
  mergeView,
  moduleFilePath,
  moduleFilesFolder,
  putModuleFile,
  readStoreFile,
  removeModuleFile,
  restartDevice,
  refreshServices,
  type InstallDeps,
  type LibraryModule,
  type TransferEvent,
  type TransferView,
} from '../modules/dktransfer';
import { RPC_SOFTWARE_VERSION, parseSoftwareVersion } from './connection-core';
import type { ConnectionApp } from './connection-state';
import type { WasmSource } from '../wasm-source';
import type { TransferEnv } from './env';
import { targetLabel, type TargetApp } from './target';

export interface BackendDeps {
  env: TransferEnv;
  target: Pick<TargetApp, 'target' | 'checkRuntime' | 'session'>;
  connection: Pick<ConnectionApp, 'asked' | 'noteConnection'>;
  /** Makes the needs reader from the app's wasm (default: patchkit.wasm in `DkNeeds`); a test gives a stub. */
  needs?: (wasm: WasmSource) => Promise<NeedsLike>;
  /**
   * Service seams (R12-7). The services come from the library: `env.library.scan` also lists `.dks` files (unbound), which the backend splits off by
   * their extension. Install and the view are service-aware only when the library has `.dks` files and `binder` and `firmwareHash` are given;
   * without them the backend behaves as before.
   */
  /** Binds an unbound `.dks` to the user's firmware (the wasm `pk_bind`, `createServiceBinder`); a test gives a fake. */
  binder?: ServiceBinder;
  /** The hash of the user's stock firmware for the device id (the key of a bound file); null when none is kept (a module that needs a service is then refused). */
  firmwareHash?: (device: string) => Promise<string | null>;
  /** The runtime of the device as the service check sees it (default: its version and the INFO `svc_abi`; a runtime that reports none is not checked). */
  runtimeIdentity?: (info: DkInfoFull) => RuntimeIdentity;
  /** The name hashes of the services the runtime provides itself (default: the generated `RUNTIME_SERVICE_NAMES`). */
  runtimeServices?: Set<number>;
}

export function createTransferBackend({
  env,
  target: targetApp,
  connection,
  needs: makeNeeds = async (wasm) => DkNeeds.create(await wasm.bytes('patchkit.wasm')),
  binder,
  firmwareHash,
  runtimeIdentity = (info) => ({ version: info.runtime, fingerprint: info.svc_abi }),
  runtimeServices = new Set(RUNTIME_SERVICE_NAMES.map(nameHash)),
}: BackendDeps) {
  const { target, checkRuntime, session: running } = targetApp;
  const { asked, noteConnection } = connection;
  let needsReader: Promise<NeedsLike> | undefined;
  /** What a .dkm takes of the device and whether it fits: patchkit.wasm's `pk_needs` and `pk_fit`, loaded once (a failed load is tried again at the next use). */
  function needs(): Promise<NeedsLike> {
    needsReader ??= makeNeeds(env.wasm)
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
  async function loadLibrary(device: string): Promise<LibraryModule[]> {
    const reader = await needs();
    const { library } = env;
    const out: LibraryModule[] = [];
    for (const f of await library.scan(device)) {
      if (f.path.endsWith(SERVICE_EXT)) continue;
      const m = await libraryModule(await library.read(device, f.path), (b) => reader.of(b));
      if (m) out.push(m);
    }
    return out;
  }

  /** Bound `.dks` files by firmware hash and unbound hash, kept for the life of the backend. */
  const bound = new Map<string, Uint8Array>();

  /** The unbound `.dks` files of the library for the device (those that are not a service file are left out). */
  async function loadServices(device: string): Promise<ServiceFile[]> {
    const { library } = env;
    const out: ServiceFile[] = [];
    for (const f of await library.scan(device)) {
      if (!f.path.endsWith(SERVICE_EXT)) continue;
      const bytes = await library.read(device, f.path);
      const stem = (f.path.split('/').pop() ?? f.path).slice(0, -SERVICE_EXT.length);
      const file = serviceFileOf(stem, bytes, fileHashHex(bytes));
      if (file) out.push(file);
    }
    return out;
  }

  /** The service-aware install and view inputs for the device, or none when the library has no service or no binder is given. */
  async function installDeps(device: string): Promise<InstallDeps | undefined> {
    if (!binder || !firmwareHash) return undefined;
    const services = await loadServices(device);
    if (services.length === 0) return undefined;
    const reader = await needs();
    return {
      services,
      binder,
      firmwareHash: await firmwareHash(device),
      runtimeIdentity,
      runtimeServices,
      cache: bound,
      needsOf: (b) => reader.of(b),
    };
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

  /** INFO, telling "no runtime" from "no reply". Only a timeout counts as no answer (E_BUSY or a protocol error prove the runtime answered,
   * so they are thrown). After a quick INFO times out, the stock SoftwareVersion (every firmware answers it, with or without the runtime)
   * says whether the device is there at all. When it is, a runtime may still be starting or busy: two more quick INFOs, 1 s apart, before
   * "no runtime". With no answer to SoftwareVersion either, the full INFO with its retries decides. */
  async function probeInfo(s: DkSession): Promise<DkInfoFull> {
    const quick = async (): Promise<DkInfoFull | undefined> => {
      try {
        return await s.info(true);
      } catch (e) {
        if (isTimeout(e)) return undefined;
        throw e;
      }
    };
    const first = await quick();
    if (first) return first;
    const os = await s.rpc(RPC_SOFTWARE_VERSION, undefined, 1500).then(parseSoftwareVersion, (e: unknown) => {
      if (isCancelled(e)) throw e;
      return undefined;
    });
    if (!os) return s.info();
    for (let i = 0; i < 2; i++) {
      await new Promise((r) => setTimeout(r, 1000));
      const again = await quick();
      if (again) return again;
    }
    throw new NoRuntimeError(
      `${targetLabel(target())} answers (OS ${os}) but runs no digikit runtime. Flash it on the Firmware page.`,
    );
  }

  /** The session, and its INFO: the answer is what the connection indicator shows. */
  async function opened() {
    await asked(async () => checkRuntime(target()));
    const s = await asked(running);
    const info = await asked(() => probeInfo(s));
    const os = await osVersion(s);
    noteConnection({
      type: 'answered',
      device: {
        id: info.port,
        name: await env.deviceName(info.port),
        ...(os ? { os } : {}),
        runtime: fmtVersion(info.runtime),
      },
    });
    return { s, info };
  }

  function libraryView(device: string, library: LibraryModule[]): TransferView {
    return { device, runtime: '', free_bytes: 0, loaded: [], capacity: null, ...mergeView(library, [], [], null, {}) };
  }

  const transferBackend = {
    /** The merged list: the library folder and what the device holds (INFO, LIST, MODULES, CAPACITY, READ). */
    async list(device: string, showLibrary: (view: TransferView) => void): Promise<TransferView> {
      const [local, remote] = await Promise.allSettled([
        loadLibrary(device).then((library) => {
          showLibrary(libraryView(device, library));
          return library;
        }),
        opened(),
      ]);
      if (remote.status === 'rejected') throw remote.reason;
      const { s, info } = remote.value;
      if (local.status === 'rejected' && info.port === device) throw local.reason;
      const library = local.status === 'fulfilled' && info.port === device ? local.value : await loadLibrary(info.port);
      const deps = await installDeps(info.port);
      return asked(() => buildView(s, library, deps));
    },
    /** The library alone, for a device id: nothing is read from a device. */
    async listLibrary(device: string): Promise<TransferView> {
      const library = await loadLibrary(device);
      return libraryView(device, library);
    },
    /** Upload and VERIFY the modules (the same file hash is not sent again), with the services they need when the library has them. The device's selection is not touched. */
    async install(modules: string[], onEvent: (e: TransferEvent) => void): Promise<void> {
      const { s, info } = await opened();
      const library = await loadLibrary(info.port);
      const deps = await installDeps(info.port);
      await asked(() => install(s, modules, library, onEvent, deps));
    },
    /**
     * The Flash job's share, once the flashed runtime answers (it may still be booting: asked again for about a minute): the services the modules on
     * the card need are bound again and replaced, the unused ones deleted (`refreshServices`). Nothing happens without services in the library.
     */
    async refreshServices(device: string, onEvent: (e: TransferEvent) => void): Promise<string[]> {
      const deps = await installDeps(device);
      if (!deps) return [];
      let last: unknown;
      for (let i = 0; i < 30; i++) {
        try {
          const { s, info } = await opened();
          const library = await loadLibrary(info.port);
          return await asked(() => refreshServices(s, deps, library, onEvent));
        } catch (e) {
          if (isCancelled(e)) throw e;
          last = e;
          await new Promise((r) => setTimeout(r, 2000));
        }
      }
      throw last;
    },
    /** DELETE the files from the card (one request each, confirmed with LIST). The device's selection is not touched. */
    async remove(paths: string[], onEvent: (e: TransferEvent) => void): Promise<void> {
      await asked(async () => checkRuntime(target()));
      const s = await asked(running);
      await asked(() => deleteFiles(s, paths, onEvent));
    },
    /** The files of a module under `data/<module id>/` on the card: name and size. */
    async listModuleFiles(moduleId: string): Promise<{ name: string; size: number }[]> {
      const { s } = await opened();
      const folder = moduleFilesFolder(moduleId);
      const entries = await asked(() => listModuleFiles(s, moduleId));
      return entries.map((e) => ({ name: e.path.slice(folder.length), size: e.size }));
    },
    /** Upload one file of a module and VERIFY it on the card. */
    async putModuleFile(
      moduleId: string,
      name: string,
      bytes: Uint8Array,
      progress: (sent: number, total: number) => void,
    ): Promise<void> {
      const s = await asked(running);
      await asked(() => putModuleFile(s, moduleId, name, bytes, progress));
    },
    /** The bytes of one file of a module on the card (READ). */
    async readModuleFile(moduleId: string, name: string): Promise<Uint8Array> {
      const s = await asked(running);
      const path = moduleFilePath(moduleId, name);
      return asked(() => readStoreFile(s, path));
    },
    /** DELETE one file of a module from the card. */
    async removeModuleFile(moduleId: string, name: string): Promise<void> {
      const s = await asked(running);
      await asked(() => removeModuleFile(s, moduleId, name));
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

  return { transferBackend, loadLibrary, installDeps };
}
export type TransferBackendApp = ReturnType<typeof createTransferBackend>;
