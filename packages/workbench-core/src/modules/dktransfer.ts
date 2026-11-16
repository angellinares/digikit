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
import {
  planDelete,
  readNeeds,
  readProvides,
  resolveServices,
  type Need,
  type ResolvedServices,
  type RuntimeIdentity,
  type ServiceBinder,
  type ServiceFile,
  type ServiceRef,
  type ServiceRefusal,
} from './services.ts';
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
  /** The services a library module needs (read-only sub rows), with their state on the card. Set only when the view was built with `InstallDeps`. */
  services?: ServiceRef[];
  /** For a `.dks` file on the card: the files on the card that hard-need it. */
  needed_by?: string[];
  /** Why the module cannot install with the services of this workbench and the runtime of the device. */
  service_refusal?: ServiceRefusal;
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
  /** The service refusals of the library modules (also on their rows); set only when the view was built with `InstallDeps`. */
  refusals?: ServiceRefusal[];
}

/** What a service-aware install, view and delete need (`install(..., deps)`): the workbench's `.dks` files and the seams to bind them to the user's firmware. */
export interface InstallDeps {
  /** The unbound `.dks` files of the workbench. */
  services: ServiceFile[];
  binder: ServiceBinder;
  /** The hash of the user's stock firmware: with the unbound file hash it keys a bound file. null: none is kept, so no file can be bound and a module that needs a service is refused (`no-stock`). */
  firmwareHash: string | null;
  runtimeIdentity: (info: DkInfoFull) => RuntimeIdentity;
  /** The name hashes of the services the runtime itself provides (no `.dks` file). */
  runtimeServices: Set<number>;
  /** Bound files by firmware hash and unbound hash; the caller keeps it between calls (no cache: each call binds again). */
  cache?: Map<string, Uint8Array>;
  /** Infers the needs of a bound `.dks` (the same reader as `libraryModule`'s `needsOf`); without it a service row has no needs and adds nothing to the fit. */
  needsOf?: (file: Uint8Array) => Needs | null;
}

/**
 * The events of install and delete. `upload`, `skip`, `progress` and `verify` of a service carry `kind: 'service'` (absent: a module); a service-aware
 * install also emits `{ event: 'refuse', refusal }` (a module that fails the service check), a delete emits `{ event: 'delete', path, why?: 'unused-service' }`
 * and `{ event: 'keep', path, by }` (a `.dks` that a module on the card still needs).
 */
export interface TransferEvent {
  event: string;
  /** Of a `refuse` event. */
  refusal?: ServiceRefusal;
  /** Of a `keep` event: the files on the card that need the service. */
  by?: string[];
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

/** What the list shows of services: the workbench's `.dks` files bound for the user's firmware, and the facts read from the card files. */
export interface ServiceView {
  services: { file: ServiceFile; bound: { size: number; hash: string }; needs?: Needs | null }[];
  /** The needs of each `.dkm` and `.dks` on the card, by path. */
  card: { path: string; needs: Need[] }[];
  /** The provided service name hash of each `.dks` on the card, by path. */
  provides: Record<string, number>;
}

/** The other files on the card that hard-need the service in the `.dks` file at `path`. */
function neededBy(sv: ServiceView, path: string): string[] {
  const hash = sv.provides[path];
  return sv.card.filter((c) => c.path !== path && c.needs.some((n) => n.hard && n.hash32 === hash)).map((c) => c.path);
}

/** The services a module needs (hard needs, and the hard needs of those services), dependencies first, among the workbench's files. */
function serviceRefs(
  m: LibraryModule,
  sv: ServiceView,
  card: Map<string, { size: number; hash: string }>,
): ServiceRef[] {
  const byHash = new Map(sv.services.map((x) => [x.file.hash32, x]));
  const out: ServiceRef[] = [];
  const seen = new Set<number>();
  const visit = (needs: Need[]): void => {
    for (const n of needs) {
      const x = n.hard ? byHash.get(n.hash32) : undefined;
      if (!x || seen.has(n.hash32)) continue;
      seen.add(n.hash32);
      visit(x.file.needs);
      const there = card.get(servicePath(x.file.stem));
      out.push({
        stem: x.file.stem,
        label: x.file.label,
        version: x.file.version,
        state: !there
          ? 'not on device'
          : there.size === x.bound.size && there.hash === x.bound.hash
            ? 'on device'
            : 'differs',
        size: x.bound.size,
        ...(x.needs ? { needs: x.needs } : {}),
      });
    }
  };
  visit(readNeeds(m.bytes));
  return out;
}

/** The list from its parts: the library, the card's LIST, the names MODULES reports loaded, CAPACITY (or null), and the header bytes of the card files.
 * With `sv` the library rows list their services and the card's `.dks` rows say which files need them. */
export function mergeView(
  library: LibraryModule[],
  card: { path: string; size: number; hash: string }[],
  loaded: string[],
  cap: Caps | null,
  headers: Record<string, Uint8Array | null>,
  states: ModuleState[] = [],
  sv?: ServiceView,
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
    if (sv) {
      const refs = serviceRefs(m, sv, onCard);
      if (refs.length) row.services = refs;
    }
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
      ...(sv && c.path.endsWith(SERVICE_EXT) && sv.provides[c.path] !== undefined
        ? { needed_by: neededBy(sv, c.path) }
        : {}),
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

/** The store folder of the services and their file extension. */
export const SERVICES_FOLDER = 'services/';
export const SERVICE_EXT = '.dks';

/** The store path of a service: `services/<stem>.dks`. */
export const servicePath = (stem: string): string => `${SERVICES_FOLDER}${stem}${SERVICE_EXT}`;

/** The service files of the store (the entries under `services/`): by prefix when the runtime has FOLDERS, else all entries, filtered here. */
export async function listServices(s: DkSession, info: DkInfoFull): Promise<DkEntry[]> {
  if (hasCap(info, 'FOLDERS')) return s.list(SERVICES_FOLDER);
  return (await s.list()).filter((e) => e.path.startsWith(SERVICES_FOLDER));
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

/** One bound file of a service: bound once per firmware hash and unbound hash (`deps.cache`). Every service goes through the binder; the binder returns a
 * service that hooks no firmware site (lfo) as built. Without a kept stock image (`firmwareHash` null) the binder gets an empty hash. */
async function bindService(deps: InstallDeps, f: ServiceFile): Promise<Uint8Array> {
  const fw = deps.firmwareHash ?? '';
  const key = `${fw}:${f.hash}`;
  const hit = deps.cache?.get(key);
  if (hit) return hit;
  const bound = await deps.binder.bind(f.bytes, fw);
  deps.cache?.set(key, bound);
  return bound;
}

/** The bound file, or null when the binder cannot make it (no kept stock image, or one patchkit cannot bind to). */
async function tryBind(deps: InstallDeps, f: ServiceFile): Promise<Uint8Array | null> {
  try {
    return await bindService(deps, f);
  } catch {
    return null;
  }
}

/** Each module whose services cannot all be bound is refused (`no-stock`) and adds no service; the other modules go on. */
async function refuseUnbound(deps: InstallDeps, plan: ResolvedServices): Promise<ResolvedServices> {
  const failed = new Set<ServiceFile>();
  for (const f of plan.order) if ((await tryBind(deps, f)) === null) failed.add(f);
  if (failed.size === 0) return plan;
  const refused = [...plan.refused];
  const byModule = new Map<string, ServiceFile[]>();
  for (const [module, files] of plan.byModule) {
    const bad = files.find((f) => failed.has(f));
    if (bad) refused.push({ module, service: bad.stem, kind: 'no-stock' });
    else byModule.set(module, files);
  }
  const kept = new Set([...byModule.values()].flat());
  return { byModule, order: plan.order.filter((f) => kept.has(f)), refused };
}

/**
 * The needs of each `.dkm` and `.dks` on the card and the service each `.dks` provides, read from the header and header table of the files (READ).
 * A library module with the same size and hash is read from the library. `known` holds header buffers already read.
 */
async function cardFacts(
  s: DkSession,
  card: DkEntry[],
  library: LibraryModule[],
  known: Record<string, Uint8Array | null> = {},
): Promise<{ needs: { path: string; needs: Need[] }[]; provides: Record<string, number>; unread: number }> {
  const mine = new Map(library.map((m) => [m.path, m]));
  const needs: { path: string; needs: Need[] }[] = [];
  const provides: Record<string, number> = {};
  let unread = 0;
  for (const c of card) {
    const isService = c.path.endsWith(SERVICE_EXT);
    if (!isService && !c.path.endsWith('.dkm')) continue;
    const m = mine.get(c.path);
    let bytes: Uint8Array | null | undefined =
      m && m.file_size === c.size && m.hash === c.hash ? m.bytes : known[c.path];
    if (bytes === undefined)
      bytes = await headerWithKeys(s, c.path).catch((e) => {
        if (e instanceof DkError && e.kind === 'device') return null;
        throw e;
      });
    if (!bytes) {
      unread++;
      continue;
    }
    needs.push({ path: c.path, needs: readNeeds(bytes) });
    const p = isService ? readProvides(bytes) : null;
    if (p) provides[c.path] = p.hash32;
  }
  return { needs, provides, unread };
}

/** The merged view for the device behind `s`. With `deps` the library rows list their services, the card's `.dks` rows say what needs them, and a module the services cannot serve carries its refusal. */
export async function buildView(s: DkSession, library: LibraryModule[], deps?: InstallDeps): Promise<TransferView> {
  const info = await s.info();
  requireRuntime(info);
  const card = [...(await listModules(s, info)), ...(await listServices(s, info))];
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
  let sv: ServiceView | undefined;
  let refused: ServiceRefusal[] = [];
  if (deps) {
    // a service the binder cannot make shows its unbound file (its modules are refused below)
    const bound: ServiceView['services'] = [];
    for (const file of deps.services) {
      const b = (await tryBind(deps, file)) ?? file.bytes;
      bound.push({ file, bound: { size: b.length, hash: fileHashHex(b) }, needs: deps.needsOf?.(b) });
    }
    const facts = hasCap(info, 'READ') ? await cardFacts(s, card, library, headers) : { needs: [], provides: {} };
    sv = { services: bound, card: facts.needs, provides: facts.provides };
    refused = (
      await refuseUnbound(
        deps,
        resolveServices(
          library.map((m) => ({ id: m.id, needs: readNeeds(m.bytes) })),
          deps.services,
          deps.runtimeIdentity(info),
          deps.runtimeServices,
        ),
      )
    ).refused;
  }
  const merged = mergeView(library, card, loaded, cap, headers, states, sv);
  for (const r of refused) {
    const row = merged.modules.find((m) => m.id === r.module);
    if (row) row.service_refusal = r;
  }
  return {
    device: info.port,
    runtime: fmtVersion(info.runtime),
    free_bytes: storeFree(info, cap).free,
    loaded,
    capacity: cap,
    ...merged,
    ...(deps ? { refusals: refused } : {}),
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
export function uploadProgress(
  emit: Emit,
  id: string,
  total: number,
  kind?: 'service',
): (sent: number, size: number) => void {
  let last = 0;
  const step = Math.max(1, Math.floor(total / PROGRESS_EVENTS));
  return (sent, size) => {
    if (sent >= size || sent - last >= step) {
      last = sent;
      emit({ event: 'progress', id, sent, bytes: size, ...(kind ? { kind } : {}) });
    }
  };
}

/**
 * For each of `ids`, in order: upload unless the card has the same file hash (a `skip` event), then VERIFY (the device hashes the file from a
 * fresh read of the card; a mismatch is an error). Nothing else: the device's selection is not touched.
 *
 * With `deps` the install is service-aware: (1) each module's NEEDS are checked against the workbench's services and the runtime; a module that
 * fails is named in a `refuse` event and left out, the others go on (when none is left, only the refusals are emitted); (2) each needed service
 * is bound to the user's firmware and uploaded to `services/<stem>.dks` (the same bound bytes on the card are skipped) and verified, before any
 * module is uploaded; (3) the modules as without `deps`; (4) a `.dks` on the card that no `.dkm` on the card hard-needs any more is deleted.
 */
export async function install(
  s: DkSession,
  ids: string[],
  library: LibraryModule[],
  emit: Emit = () => {},
  deps?: InstallDeps,
): Promise<{ uploaded: string[]; selected: string[]; restarted: false; refused?: ServiceRefusal[] }> {
  const info = await s.info();
  requireRuntime(info);
  const by = new Map(library.map((m) => [m.id, m]));
  const missing = ids.filter((i) => !by.has(i));
  if (missing.length)
    throw new DkError({
      error: `not a run-time module that packs for this device: ${missing.join(', ')}`,
      kind: 'request',
    });
  const canVerify = hasCap(info, 'VERIFY');
  let order: ServiceFile[] = [];
  let refused: ServiceRefusal[] = [];
  if (deps) {
    const plan = await refuseUnbound(
      deps,
      resolveServices(
        ids.map((id) => ({ id, needs: readNeeds(by.get(id)!.bytes) })),
        deps.services,
        deps.runtimeIdentity(info),
        deps.runtimeServices,
      ),
    );
    refused = plan.refused;
    for (const refusal of refused) emit({ event: 'refuse', refusal });
    const bad = new Set(refused.map((r) => r.module));
    ids = ids.filter((id) => !bad.has(id));
    if (ids.length === 0) return { uploaded: [], selected: [], restarted: false, refused };
    order = plan.order;
  }
  emit({ event: 'start', device: info.port, modules: ids });
  if (deps && order.length) {
    // bind every service first: a failure here uploads nothing
    const bound = new Map<string, Uint8Array>();
    for (const f of order) bound.set(f.stem, await bindService(deps, f));
    const onCard = new Map((await listServices(s, info)).map((c) => [c.path, c]));
    for (const f of order) {
      const path = servicePath(f.stem);
      const bytes = bound.get(f.stem)!;
      const hash = fileHashHex(bytes);
      emit({ event: 'upload', id: f.stem, kind: 'service', path, bytes: bytes.length });
      const there = onCard.get(path);
      if (there && there.size === bytes.length && there.hash === hash) {
        emit({ event: 'skip', id: f.stem, kind: 'service', reason: 'same hash on the card' });
      } else {
        await s.put(path, bytes, uploadProgress(emit, f.stem, bytes.length, 'service'));
      }
      if (canVerify) {
        const got = await s.verify(path);
        const ok = got === hash;
        emit({ event: 'verify', id: f.stem, kind: 'service', path, ok });
        if (!ok)
          throw new DkError({
            error: `${path}: the device reads back hash ${got.slice(0, 8)}, expected ${hash.slice(0, 8)}: the card did not keep the file`,
            kind: 'verify',
          });
      }
    }
  }
  const have = new Map((await listModules(s, info)).map((c) => [c.path, c]));
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
  if (deps && canReadHeaders(info)) await removeUnusedServices(s, info, library, emit);
  emit({ event: 'done', restarted: false, selected: [] });
  return deps ? { uploaded, selected: [], restarted: false, refused } : { uploaded, selected: [], restarted: false };
}

/**
 * The Flash job's share (`firmware-state.ts`, after the runtime answers): each `.dks` on the card that the workbench has a file for is bound again and replaced when the card holds other bytes
 * (a flashed runtime can carry another service ABI). A file the workbench has no service for, or cannot bind, stays; a failed upload or verify stops the run.
 * Then each `.dks` that no module on the card needs is deleted. Returns the paths replaced.
 */
export async function refreshServices(
  s: DkSession,
  deps: InstallDeps,
  library: LibraryModule[],
  emit: Emit = () => {},
): Promise<string[]> {
  const info = await s.info();
  requireRuntime(info);
  const mine = new Map(deps.services.map((f) => [servicePath(f.stem), f]));
  const replaced: string[] = [];
  for (const c of await listServices(s, info)) {
    const f = mine.get(c.path);
    const bytes = f && (await tryBind(deps, f));
    if (!f || !bytes) continue;
    const hash = fileHashHex(bytes);
    if (c.size === bytes.length && c.hash === hash) continue;
    emit({ event: 'upload', id: f.stem, kind: 'service', path: c.path, bytes: bytes.length });
    await s.put(c.path, bytes, uploadProgress(emit, f.stem, bytes.length, 'service'));
    if (hasCap(info, 'VERIFY')) {
      const got = await s.verify(c.path);
      emit({ event: 'verify', id: f.stem, kind: 'service', path: c.path, ok: got === hash });
      if (got !== hash)
        throw new DkError({
          error: `${c.path}: the device reads back hash ${got.slice(0, 8)}, expected ${hash.slice(0, 8)}: the card did not keep the file`,
          kind: 'verify',
        });
    }
    replaced.push(c.path);
  }
  // and the stale ones go: no module on the card needs them
  if (canReadHeaders(info)) await removeUnusedServices(s, info, library, emit);
  return replaced;
}

/** A `.dks` that no `.dkm` on the card hard-needs (directly or through another service) is not wanted any more: delete it. Nothing is deleted when a header cannot be read. */
async function removeUnusedServices(
  s: DkSession,
  info: DkInfoFull,
  library: LibraryModule[],
  emit: Emit,
): Promise<void> {
  const card = [...(await listModules(s, info)), ...(await listServices(s, info))];
  const facts = await cardFacts(s, card, library);
  const wanted = neededHashes(facts.needs, facts.provides);
  for (const c of facts.unread ? [] : card) {
    const hash = facts.provides[c.path];
    if (hash === undefined || wanted.has(hash)) continue;
    emit({ event: 'delete', path: c.path, why: 'unused-service' });
    await s.delete(c.path);
  }
}

/** Whether the device answers READ (the headers of its card files can be read). */
const canReadHeaders = (info: DkInfoFull): boolean => hasCap(info, 'READ');

/** The service name hashes the files on the card hard-need: those of the `.dkm` files, and of the `.dks` files they need, and so on. */
function neededHashes(needs: { path: string; needs: Need[] }[], provides: Record<string, number>): Set<number> {
  const wanted = new Set<number>();
  const byHash = new Map(Object.entries(provides).map(([path, hash]) => [hash, path]));
  const queue = needs.filter((n) => n.path.endsWith('.dkm')).flatMap((n) => n.needs);
  while (queue.length) {
    const n = queue.pop()!;
    if (!n.hard || wanted.has(n.hash32)) continue;
    wanted.add(n.hash32);
    const path = byHash.get(n.hash32);
    if (path) queue.push(...(needs.find((x) => x.path === path)?.needs ?? []));
  }
  return wanted;
}

/** The name of a file for a person: `modules/pluck.dkm` is `pluck`, `services/fx.dks` is `fx`. */
const leafName = (path: string): string => (path.split('/').pop() ?? path).replace(/\.dk[ms]$/, '');

/**
 * DELETE each path (one request each), then LIST to confirm each is gone. A path that is not on the card is an error before anything is sent.
 * When the card has `.dks` files (and READ works) the delete is planned first: deleting a module also deletes each `.dks` that no remaining module
 * hard-needs (`delete` event with `why: 'unused-service'`), a `.dks` that a remaining module needs is kept (`keep` event), and a needed `.dks` named
 * alone is refused before any DELETE. A failure half way names the files deleted and the files left.
 */
export async function deleteFiles(
  s: DkSession,
  paths: string[],
  emit: Emit = () => {},
): Promise<{ deleted: string[] }> {
  const info = await s.info();
  requireRuntime(info);
  const want = [...new Set(paths)];
  const entries = await s.list();
  const onCard = new Set(entries.map((c) => c.path));
  const missing = want.filter((p) => !onCard.has(p));
  if (missing.length) throw new DkError({ error: `not on the card: ${missing.join(', ')}`, kind: 'request' });
  let extra: string[] = [];
  let kept: { path: string; by: string[] }[] = [];
  const dkFiles = entries.filter((c) => c.path.endsWith(SERVICE_EXT) || c.path.endsWith('.dkm'));
  if (hasCap(info, 'READ') && dkFiles.some((c) => c.path.endsWith(SERVICE_EXT))) {
    const facts = await cardFacts(s, dkFiles, []);
    if (!facts.unread) {
      const plan = planDelete(facts.needs, want);
      if (plan.refused.length)
        throw new DkError({
          error: plan.refused
            .map(
              (r) =>
                `${leafName(r.path)} service is needed by ${r.by.map(leafName).join(', ')}. Delete that module first (its services go with it).`,
            )
            .join(' '),
          kind: 'request',
        });
      extra = plan.delete.filter((p) => !want.includes(p));
      kept = plan.kept;
    }
  }
  emit({ event: 'start', device: info.port, paths: want });
  for (const k of kept) emit({ event: 'keep', path: k.path, by: k.by });
  const all = [...want, ...extra];
  const done: string[] = [];
  for (const path of all) {
    emit({ event: 'delete', path, ...(extra.includes(path) ? { why: 'unused-service' } : {}) });
    try {
      await s.delete(path);
    } catch (e) {
      const left = all.filter((p) => !done.includes(p));
      throw new DkError({
        error: `${e instanceof Error ? e.message : String(e)}. Deleted: ${done.join(', ') || 'nothing'}. Not deleted: ${left.join(', ')}`,
        kind: e instanceof DkError ? e.kind : 'device',
      });
    }
    done.push(path);
  }
  const after = new Set((await s.list()).map((c) => c.path));
  const still = all.filter((p) => after.has(p));
  if (still.length) throw new DkError({ error: `still on the card after DELETE: ${still.join(', ')}`, kind: 'device' });
  emit({ event: 'done', deleted: all });
  return { deleted: all };
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
