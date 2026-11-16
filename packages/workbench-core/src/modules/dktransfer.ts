// The Transfer logic in TypeScript (the port of tools/device/dktransfer.py, which stays as the command line for scripts and the
// scenarios): the merged list of the built modules in a folder and what the device holds, install (upload, skip the same file hash,
// VERIFY), delete (DELETE, confirmed by LIST) and restart (RESTART, wait for the boot, report what loaded). The events have the shapes
// the Python tool prints with --json. Written without TS-only runtime syntax so node can import it for the tests.
//
// The app does not build: its modules are built `.dkm` files of a folder (the workbench app builds and packs them into out/modules/<device>/ when it reads the library: `lib_build`, apps/desktop/src-tauri/src/library.rs).
import {
  DkError,
  DkSession,
  HEADER_BYTES,
  H_ROLE,
  RECORD_KIND,
  TABLE_MAGIC,
  TABLE_VERSION,
  fileHashHex,
  fmtVersion,
  hasCap,
  moduleStateText,
  storeFree,
  type ModuleState,
  type ModuleStateName,
} from '@digi/dk-session';
import { fitsNow, type Caps, type Needs } from './fit.ts';
import type { DkEntry, DkInfoFull } from '@digi/dk-session';

export type Group = 'Machines' | 'Effects' | 'Tools' | 'Services';

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
  /** Why the module is not loaded although the last boot selected it (`moduleStateText`): the boot plan's refusal or the load's failure; set only for such a file. */
  not_loaded?: string;
  /** The module's state at the last boot (REFUSED, FAILED or SKIPPED) when `not_loaded` is set. */
  load_state?: ModuleStateName;
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
const KEYS_TAG = 3; // the KEYS entry of the header table (packages/dkneeds/src/keys.rs)

/** Readers accept the header table versions 1 to TABLE_VERSION (a service is written with 2, a module with 1). */
const tableVersionOk = (version: number): boolean => version >= 1 && version <= TABLE_VERSION;

/** The group of a .dkm: Services when its role word (header 0x38) is 1, Machines when the KEYS entry of its header table (the header's 0x90 pair) has machine keys, Effects when it has effect keys, else Tools.
 * Without the table (a header alone, or the table of runtime 0.7) it is Tools. */
export function groupOf(f: Uint8Array): Group {
  const v = new DataView(f.buffer, f.byteOffset, f.length);
  if (f.length < HEADER_BYTES) return 'Tools';
  const off = v.getUint32(0x90);
  const size = v.getUint32(0x94);
  if (size < 8 || off + size > f.length || v.getUint32(off) !== TABLE_MAGIC || !tableVersionOk(v.getUint16(off + 4)))
    return 'Tools';
  if (v.getUint32(H_ROLE) === 1) return 'Services';
  const kinds = new Set<number>();
  let at = off + 8;
  for (let i = v.getUint16(off + 6); i > 0 && at + 8 <= off + size; i--) {
    const tag = v.getUint16(at);
    const count = v.getUint16(at + 2);
    const len = v.getUint32(at + 4);
    if (tag === KEYS_TAG) {
      let g = at + 8;
      for (let j = 0; j < count && g + 8 <= off + size; j++) {
        const n = v.getUint32(g + 4);
        if (n) kinds.add(v.getUint32(g));
        g += 8 + 4 * n;
      }
    }
    at += 8 + len;
  }
  if (kinds.has(RECORD_KIND.MACHINE)) return 'Machines';
  if (kinds.has(RECORD_KIND.EFFECT)) return 'Effects';
  return 'Tools';
}

/** What to tell a person about a file with no header table or another table version: it was built for an older runtime (the loader's E_CONTRACT). */
export const OLDER_RUNTIME_TEXT = 'module built for an older runtime: rebuild it';

/** Whether the bytes (the header and the table behind it, as `groupOf` reads them) carry the header table of this runtime; false means `OLDER_RUNTIME_TEXT`. */
export function hasHeaderTable(f: Uint8Array): boolean {
  if (f.length < HEADER_BYTES) return false;
  const v = new DataView(f.buffer, f.byteOffset, f.length);
  const off = v.getUint32(0x90);
  const size = v.getUint32(0x94);
  return (
    size >= 8 && off + size <= f.length && v.getUint32(off) === TABLE_MAGIC && tableVersionOk(v.getUint16(off + 4))
  );
}

/** The header fields of a .dkm (its first 0xa0 bytes, or the whole file for the group); null when the bytes are not one. */
export function headerInfo(h: Uint8Array): HeaderInfo | null {
  if (h.length < HEADER_BYTES || MAGIC.some((b, i) => h[i] !== b)) return null;
  const v = new DataView(h.buffer, h.byteOffset, h.length);
  return {
    name: new TextDecoder().decode(h.subarray(0x40, 0x60)).split('\0')[0]!,
    version: fmtVersion(v.getUint32(0x34)),
    group: groupOf(h),
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
  const h = headerInfo(bytes);
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

export const GROUP_ORDER: Group[] = ['Machines', 'Effects', 'Tools', 'Services'];

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
  states: ModuleState[] = [],
): Pick<TransferView, 'modules' | 'card_only'> {
  /** The refusal or load failure of the file at `path` (the runtime keeps the file name, 27 characters at most). */
  const whyNot = (path: string, isLoaded: boolean): { not_loaded?: string; load_state?: ModuleStateName } => {
    const base = (path.split('/').pop() ?? path).slice(0, 27);
    const f = isLoaded ? undefined : states.find((x) => x.name === base);
    const text = f && moduleStateText(f);
    return f && text ? { not_loaded: text, load_state: f.state } : {};
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
      // a file without the header table of this runtime was built for an older one
      ...(h && !hasHeaderTable(headers[c.path]!) ? { warnings: [OLDER_RUNTIME_TEXT] } : {}),
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

/** The store folder of a module's own files (sdk/include/dk/platform/data.h): `data/<module id>/`. */
export const moduleFilesFolder = (moduleId: string): string => `data/${moduleId}/`;

/** The module chooser keeps a file name in 48 bytes, NUL included (modules/tools/bootscreen/src/module.c, NAME_MAX_BYTES). */
export const MODULE_FILE_NAME_LIMIT = 48;

/**
 * The store path of a file of a module: `data/<module id>/<name>`. The rule of `dk_data_path` (sdk/include/dk/platform/data.h): `name` is not empty
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
  const states = hasCap(info, 'LOADS') ? await s.loadStates() : [];
  const mine = new Set(library.map((m) => m.path));
  const headers: Record<string, Uint8Array | null> = {};
  if (hasCap(info, 'READ')) {
    for (const c of card) {
      if (mine.has(c.path)) continue;
      headers[c.path] = await headerWithKeys(s, c.path).catch((e) => {
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
    ...mergeView(library, card, loaded, cap, headers, states),
  };
}

/** The header of a store file and its keys table (header 0x90/0x94) at their offsets in a buffer, which `groupOf` reads; the rest is zero. */
async function headerWithKeys(s: DkSession, path: string): Promise<Uint8Array> {
  const h = await s.header(path);
  if (h.length < HEADER_BYTES) return h;
  const v = new DataView(h.buffer, h.byteOffset, h.length);
  const off = v.getUint32(0x90);
  const size = v.getUint32(0x94);
  if (size < 8 || size > 4096 || off < HEADER_BYTES) return h;
  const t = (await s.read(path, off, size)).bytes;
  const out = new Uint8Array(off + t.length);
  out.set(h);
  out.set(t, off);
  return out;
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
  opts: { signal?: AbortSignal; stillBooting?: () => boolean } = {},
): Promise<{ loaded: string[]; missing: string[] }> {
  const info = await s.info();
  requireRuntime(info);
  emit({ event: 'restart' });
  await s.restart();
  emit({ event: 'wait', modules: [] });
  await s.waitForBoot(sleep, undefined, opts);
  // MODULES answers once the runtime is up; the modules of the selection load at the start of the boot
  const end = Date.now() + loadedTimeoutMs;
  let loaded: string[] = [];
  for (;;) {
    if (opts.signal?.aborted) throw new DkError({ error: 'cancelled', kind: 'cancelled' });
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
