// The Transfer logic in TypeScript (the port of tools/device/dktransfer.py, which stays as the command line for scripts and the
// scenarios): the merged list of the built modules in a folder and what the device holds, install (upload, skip the same file hash,
// VERIFY), delete (DELETE, confirmed by LIST) and restart (RESTART, wait for the boot, report what loaded). The events have the shapes
// the Python tool prints with --json. Written without TS-only runtime syntax so node can import it for the tests.
//
// The app does not build: its modules are built `.dkm` files of a folder (`mise run module:pack-all`).
import {
  DkError,
  DkSession,
  HEADER_BYTES,
  fileHashHex,
  fmtVersion,
  hasCap,
  loadFailureText,
  storeFree,
  type LoadFailure,
} from '@digi/dk-session';
import { fitsNow, type Caps, type Needs } from './fit.ts';
import type { DkEntry, DkInfoFull } from '@digi/dk-session';

export type Group = 'Machines' | 'Effects' | 'Tools';

/** One row of the list: a built module (`id` set) or a card file that matches none (`id` null). */
export interface TransferRow {
  id: string | null;
  path: string;
  name: string;
  label?: string;
  group: Group;
  test: boolean;
  version: string | null;
  file_size: number;
  arena?: number;
  dsp?: number;
  /** What a load of the module takes, inferred from its contents (docs/design/digikit-dkm.md, "Needs are inferred"); `file_size` is its size. */
  needs?: Needs;
  on_card: 'yes' | 'no' | 'other version';
  loaded: boolean;
  /** Why the module is not loaded although the last boot selected it (`loadFailureText`); set only for a file that failed to load. */
  not_loaded?: string;
  fits?: boolean | null;
  warnings?: string[];
  known?: boolean;
}

export interface TransferView {
  device: string;
  runtime: string;
  free_bytes: number;
  loaded: string[];
  /** CAPACITY of the device (null: it has none): the figures the fit of each module is judged by. */
  capacity?: Caps | null;
  modules: TransferRow[];
  card_only: TransferRow[];
}

export interface TransferEvent {
  event: string;
  [k: string]: unknown;
}
export type Emit = (e: TransferEvent) => void;

/** The fields of a .dkm header (docs/design/digikit-dkm.md) the list shows. */
export interface HeaderInfo {
  name: string;
  version: string;
  contracts: { api: [number, number]; machine: [number, number]; fx: [number, number]; service: [number, number] };
  group: Group;
  size: number;
}

/** A built module of the library folder: its header fields, its needs (none when the loader would refuse the file), its hash and its bytes. */
export interface LibraryModule extends HeaderInfo {
  needs: Needs | null;
  /** The module id (the header's name: `<author/name>`). */
  id: string;
  label: string;
  test: boolean;
  /** The store path the file is uploaded to: `modules/<last part of the id>.dkm`. */
  path: string;
  /** `fileHashHex` of the file: what the store's LIST and VERIFY say of it. */
  hash: string;
  file_size: number;
  bytes: Uint8Array;
}

const MAGIC = [0x44, 0x4b, 0x4d, 0x46]; // "DKMF"

/** Machine contract: Machines; fx: Effects; service or api only: Tools. */
export function groupOf(c: HeaderInfo['contracts']): Group {
  if (c.machine[0]) return 'Machines';
  if (c.fx[0]) return 'Effects';
  return 'Tools';
}

/** The header fields of a .dkm (its first 0xa0 bytes); null when the bytes are not one. */
export function headerInfo(h: Uint8Array): HeaderInfo | null {
  if (h.length < HEADER_BYTES || MAGIC.some((b, i) => h[i] !== b)) return null;
  const v = new DataView(h.buffer, h.byteOffset, h.length);
  const pair = (at: number): [number, number] => [h[at]!, h[at + 1]!];
  const contracts = { api: pair(0x38), machine: pair(0x3a), fx: pair(0x3c), service: pair(0x3e) };
  return {
    name: new TextDecoder().decode(h.subarray(0x40, 0x60)).split('\0')[0]!,
    version: fmtVersion(v.getUint32(0x34)),
    contracts,
    group: groupOf(contracts),
    size: v.getUint32(8),
  };
}

/** The store path of a module id. */
export const modulePath = (id: string): string => `modules/${id.split('/').pop()}.dkm`;

/** A `LibraryModule` from the bytes of a built .dkm; null when they are not one. `needsOf` infers the needs from the whole file (`DkNeeds.of`, dkneeds.ts:
 * patchkit.wasm); without it the module has none and no fit verdict. */
export async function libraryModule(
  bytes: Uint8Array,
  needsOf?: (file: Uint8Array) => Needs | null,
): Promise<LibraryModule | null> {
  const h = headerInfo(bytes.subarray(0, HEADER_BYTES));
  if (!h) return null;
  return {
    ...h,
    needs: needsOf?.(bytes) ?? null,
    id: h.name,
    label: h.name.split('/').pop() ?? h.name,
    test: h.name.startsWith('test/'),
    path: modulePath(h.name),
    hash: fileHashHex(bytes),
    file_size: bytes.length,
    bytes,
  };
}

export const GROUP_ORDER: Group[] = ['Machines', 'Effects', 'Tools'];

/**
 * What would not fit on the target (CAPACITY): `[]` when it fits next to what is loaded now (the fit rule, fit.ts), else one line that says what is
 * short and whether the module fits the device when the loaded ones are left out (the next boot loads the selection into an empty runtime).
 * Only a warning: the device refuses a load that does not fit.
 */
export function fitWarnings(row: { loaded: boolean; needs?: Needs }, cap: Caps | null): string[] {
  if (!cap || row.loaded || !row.needs) return [];
  const now = fitsNow(row.needs, cap);
  if (!now) return [];
  const alone = fitsNow(row.needs, cap, true) === null;
  return [
    `${now.describe_by}${alone ? '; fits when the modules loaded now are not selected' : '; more than this device has'}`,
  ];
}

/** The list from its parts: the library, the card's LIST, the names MODULES reports loaded, CAPACITY (or null), and the header bytes of the card files. */
export function mergeView(
  library: LibraryModule[],
  card: { path: string; size: number; hash: string }[],
  loaded: string[],
  cap: Caps | null,
  headers: Record<string, Uint8Array | null>,
  failures: LoadFailure[] = [],
): Pick<TransferView, 'modules' | 'card_only'> {
  /** The load failure of the file at `path` (the runtime keeps the file name, 27 characters at most). */
  const whyNot = (path: string, isLoaded: boolean): { not_loaded?: string } => {
    const base = (path.split('/').pop() ?? path).slice(0, 27);
    const f = isLoaded ? undefined : failures.find((x) => x.name === base);
    return f ? { not_loaded: loadFailureText(f) } : {};
  };
  const onCard = new Map(card.map((c) => [c.path, c]));
  const seen = new Set<string>();
  const modules: TransferRow[] = library.map((m) => {
    const have = onCard.get(m.path);
    seen.add(m.path);
    const row: TransferRow = {
      id: m.id,
      path: m.path,
      name: m.name,
      label: m.label,
      group: m.group,
      test: m.test,
      version: m.version,
      file_size: m.file_size,
      arena: m.needs?.ARENA,
      dsp: m.needs?.DSP_AREA,
      needs: m.needs ?? undefined,
      on_card: !have ? 'no' : have.size === m.file_size && have.hash === m.hash ? 'yes' : 'other version',
      loaded: loaded.includes(m.name),
      ...whyNot(m.path, loaded.includes(m.name)),
    };
    row.warnings = fitWarnings(row, cap);
    row.fits = cap === null ? null : row.warnings.length === 0;
    return row;
  });
  modules.sort(
    (a, b) =>
      GROUP_ORDER.indexOf(a.group) - GROUP_ORDER.indexOf(b.group) ||
      Number(a.test) - Number(b.test) ||
      (a.id ?? '').localeCompare(b.id ?? ''),
  );
  const card_only: TransferRow[] = [];
  for (const c of card) {
    if (seen.has(c.path)) continue;
    const h = headerInfo(headers[c.path] ?? new Uint8Array());
    card_only.push({
      id: null,
      path: c.path,
      name: h?.name || (c.path.split('/').pop() ?? c.path),
      group: h?.group ?? 'Tools',
      known: h !== null,
      version: h?.version ?? null,
      file_size: c.size,
      test: false,
      on_card: 'yes',
      loaded: h ? loaded.includes(h.name) : false,
      ...whyNot(c.path, h ? loaded.includes(h.name) : false),
    });
  }
  return { modules, card_only };
}

/** The runtime must answer with the upload protocol of runtime 4 (SELECT, RESTART) and write the card. */
export function requireRuntime(info: DkInfoFull): void {
  if (info.runtime < 4)
    throw new DkError({
      error: `the device runs runtime ${fmtVersion(info.runtime)}; Transfer needs 0.0.4 or newer (flash a runtime image)`,
      kind: 'request',
    });
  if (info.card_read_only)
    throw new DkError({
      error:
        "the card's store area is not ours and not blank: the runtime writes nothing there (ABOUT shows CARD READ-ONLY)",
      kind: 'request',
    });
}

/** The store folder of the modules; a store of version 3 also has `samples/` (and the Samples mode lists that one). */
export const MODULES_FOLDER = 'modules/';

/** The module files of the store (the entries under `modules/`): by prefix when the runtime has FOLDERS, else all entries, filtered here. */
export async function listModules(s: DkSession, info: DkInfoFull): Promise<DkEntry[]> {
  if (hasCap(info, 'FOLDERS')) return s.list(MODULES_FOLDER);
  return (await s.list()).filter((e) => e.path.startsWith(MODULES_FOLDER));
}

/** The store folder of a module's own files (sdk/include/dk/data.h): `data/<module id>/`. */
export const moduleFilesFolder = (moduleId: string): string => `data/${moduleId}/`;

/** The module chooser keeps a file name in 48 bytes, NUL included (modules/tools/bootscreen/src/module.c, NAME_MAX_BYTES). */
export const MODULE_FILE_NAME_LIMIT = 48;

/**
 * The store path of a file of a module: `data/<module id>/<name>`. The rule of `dk_data_path` (sdk/include/dk/data.h): `name` is not empty
 * and holds neither '/' nor "..". Throws otherwise.
 */
export function moduleFilePath(moduleId: string, name: string): string {
  if (name.length >= MODULE_FILE_NAME_LIMIT)
    throw new DkError({
      error: `"${name}" is ${name.length} characters; a module file name must be shorter than ${MODULE_FILE_NAME_LIMIT}`,
      kind: 'request',
    });
  if (!name || name.includes('/') || name.includes('..'))
    throw new DkError({ error: `"${name}" is not a name for a module file (empty, '/' or '..')`, kind: 'request' });
  return `${moduleFilesFolder(moduleId)}${name}`;
}

/** The files of a module under `data/<module id>/`: by prefix when the runtime has FOLDERS, else all entries, filtered here. */
export async function listModuleFiles(s: DkSession, moduleId: string): Promise<DkEntry[]> {
  const folder = moduleFilesFolder(moduleId);
  const info = await s.info();
  requireRuntime(info);
  const all = hasCap(info, 'FOLDERS') ? await s.list(folder) : await s.list();
  return all.filter((e) => e.path.startsWith(folder) && e.path.length > folder.length);
}

/** Upload one file of a module and VERIFY it against the bytes sent (a mismatch is an error). */
export async function putModuleFile(
  s: DkSession,
  moduleId: string,
  name: string,
  bytes: Uint8Array,
  progress: (sent: number, total: number) => void = () => {},
): Promise<void> {
  const path = moduleFilePath(moduleId, name);
  requireRuntime(await s.info());
  await s.put(path, bytes, progress);
  if (!(await s.verifyFile(path, bytes)))
    throw new DkError({
      error: `${path}: the device reads back other bytes: the card did not keep the file`,
      kind: 'verify',
    });
}

/** DELETE one file of a module. */
export async function removeModuleFile(s: DkSession, moduleId: string, name: string): Promise<void> {
  const path = moduleFilePath(moduleId, name);
  requireRuntime(await s.info());
  await s.delete(path);
}

/** The whole file at `path` of the store, read in pieces of READ (runtime 0.2.4, capability READ). */
export async function readStoreFile(s: DkSession, path: string, piece = 768): Promise<Uint8Array> {
  const first = await s.read(path, 0, piece);
  const out = new Uint8Array(first.size);
  out.set(first.bytes.subarray(0, first.size));
  let at = first.bytes.length;
  while (at < first.size) {
    const part = await s.read(path, at, Math.min(piece, first.size - at));
    if (part.bytes.length === 0) throw new DkError({ error: `${path}: short read at ${at}`, kind: 'protocol' });
    out.set(part.bytes, at);
    at += part.bytes.length;
  }
  return out;
}

/** The merged view for the device behind `s`. */
export async function buildView(s: DkSession, library: LibraryModule[]): Promise<TransferView> {
  const info = await s.info();
  requireRuntime(info);
  const card = await listModules(s, info);
  const loaded = hasCap(info, 'MODULES') ? (await s.modules()).map((m) => m.name) : [];
  const cap = hasCap(info, 'CAPACITY') ? await s.capacity() : null;
  const failures = hasCap(info, 'LOADS') ? await s.loadFailures() : [];
  const mine = new Set(library.map((m) => m.path));
  const headers: Record<string, Uint8Array | null> = {};
  if (hasCap(info, 'READ')) {
    for (const c of card) {
      if (mine.has(c.path)) continue;
      headers[c.path] = await s.header(c.path).catch((e) => {
        if (e instanceof DkError && e.kind === 'device') return null;
        throw e;
      });
    }
  }
  return {
    device: info.port,
    runtime: fmtVersion(info.runtime),
    free_bytes: storeFree(info, cap).free,
    loaded,
    capacity: cap,
    ...mergeView(library, card, loaded, cap, headers, failures),
  };
}

/** The most `progress` events one file's upload emits, plus the one at 100 %. */
export const PROGRESS_EVENTS = 10;

/** The progress callback of one file's upload: emits `progress` when a tenth of the file went since the last event, and at 100 %. */
export function uploadProgress(emit: Emit, id: string, total: number): (sent: number, size: number) => void {
  let last = 0;
  const step = Math.max(1, Math.floor(total / PROGRESS_EVENTS));
  return (sent, size) => {
    if (sent >= size || sent - last >= step) {
      last = sent;
      emit({ event: 'progress', id, sent, bytes: size });
    }
  };
}

/**
 * For each of `ids`, in order: upload unless the card has the same file hash (a `skip` event), then VERIFY (the device hashes the file from a
 * fresh read of the card; a mismatch is an error). Nothing else: the device's selection is not touched.
 */
export async function install(
  s: DkSession,
  ids: string[],
  library: LibraryModule[],
  emit: Emit = () => {},
): Promise<{ uploaded: string[]; selected: string[]; restarted: false }> {
  const info = await s.info();
  requireRuntime(info);
  const by = new Map(library.map((m) => [m.id, m]));
  const missing = ids.filter((i) => !by.has(i));
  if (missing.length)
    throw new DkError({
      error: `not a run-time module that packs for this device: ${missing.join(', ')}`,
      kind: 'request',
    });
  emit({ event: 'start', device: info.port, modules: ids });
  const have = new Map((await listModules(s, info)).map((c) => [c.path, c]));
  const canVerify = hasCap(info, 'VERIFY');
  const uploaded: string[] = [];
  for (const id of ids) {
    const m = by.get(id)!;
    emit({ event: 'upload', id, path: m.path, bytes: m.file_size });
    const there = have.get(m.path);
    if (there && there.size === m.file_size && there.hash === m.hash) {
      emit({ event: 'skip', id, reason: 'same hash on the card' });
    } else {
      const progress = uploadProgress(emit, id, m.file_size);
      await s.put(m.path, m.bytes, progress);
      uploaded.push(id);
    }
    if (canVerify) {
      const got = await s.verify(m.path);
      const ok = got === m.hash;
      emit({ event: 'verify', id, path: m.path, ok });
      if (!ok)
        throw new DkError({
          error: `${m.path}: the device reads back hash ${got.slice(0, 8)}, expected ${m.hash.slice(0, 8)}: the card did not keep the file`,
          kind: 'verify',
        });
    }
  }
  emit({ event: 'done', restarted: false, selected: [] });
  return { uploaded, selected: [], restarted: false };
}

/** DELETE each path (one request each), then LIST to confirm each is gone. A path that is not on the card is an error before anything is sent. */
export async function deleteFiles(
  s: DkSession,
  paths: string[],
  emit: Emit = () => {},
): Promise<{ deleted: string[] }> {
  const info = await s.info();
  requireRuntime(info);
  const want = [...new Set(paths)];
  const onCard = new Set((await s.list()).map((c) => c.path));
  const missing = want.filter((p) => !onCard.has(p));
  if (missing.length) throw new DkError({ error: `not on the card: ${missing.join(', ')}`, kind: 'request' });
  emit({ event: 'start', device: info.port, paths: want });
  for (const path of want) {
    emit({ event: 'delete', path });
    await s.delete(path);
  }
  const left = new Set((await s.list()).map((c) => c.path));
  const still = want.filter((p) => left.has(p));
  if (still.length) throw new DkError({ error: `still on the card after DELETE: ${still.join(', ')}`, kind: 'device' });
  emit({ event: 'done', deleted: want });
  return { deleted: want };
}

/**
 * RESTART, wait for the boot (the log), then report what MODULES lists. The page expects no module: it only reports what loaded.
 */
export async function restartDevice(
  s: DkSession,
  emit: Emit = () => {},
  sleep: (ms: number) => Promise<void> = (ms) => new Promise((r) => setTimeout(r, ms)),
  loadedTimeoutMs = 20000,
): Promise<{ loaded: string[]; missing: string[] }> {
  const info = await s.info();
  requireRuntime(info);
  emit({ event: 'restart' });
  await s.restart();
  emit({ event: 'wait', modules: [] });
  await s.waitForBoot(sleep);
  // MODULES answers once the runtime is up; the modules of the selection load at the start of the boot
  const end = Date.now() + loadedTimeoutMs;
  let loaded: string[] = [];
  for (;;) {
    try {
      loaded = (await s.modules()).map((m) => m.name);
      break;
    } catch (e) {
      if (Date.now() >= end) throw e;
      await sleep(500);
    }
  }
  emit({ event: 'done', restarted: true, loaded, missing: [] });
  return { loaded, missing: [] };
}
