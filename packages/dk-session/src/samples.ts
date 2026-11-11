// Samples on a device, one API over two backends (docs/plans/storage.md). The device profile's `[storage] samples` names the backend:
//   "stock-rpc"  the device's own MidiRpc file commands, the +Drive its sample browser lists (rpcfiles.ts; the Digitakt II)
//   "digikit"    the `samples/` folder of the digikit store, the runtime's SysEx commands (`DkSession`; the Digitone II)
// Both take the file `prepareUpload` (dksample.ts) makes, the same native sample file, so a sampling engine reads one format. The result of a verify has one
// shape on both: what the device keeps against what the host sent. Written without TS-only runtime syntax so node can import it.
import type { BulkSession } from './bulk.ts';
import { BulkDeviceError } from './bulk.ts';
import { DkError, concat, fileHash, fileHashHex, hasCap, storeSpace } from './dkupload-core.ts';
import type { DkEntry, DkInfoFull, DkSession } from './dkupload-core.ts';
import type { Prepared } from './dksample.ts';
import { RpcFiles, TRAILER, verifyWritten } from './rpcfiles.ts';
import type { RpcPort } from './rpcfiles.ts';

export type SamplesKind = 'stock-rpc' | 'digikit';

/** A file or a folder of the listing; `size` is what the device reports. `hash` is the device's content hash of a file as the backend lists it
 * (opaque: only `sameOnDevice` of the same backend reads it); absent for a folder. */
export interface SampleEntry {
  name: string;
  kind: 'file' | 'folder';
  size: number;
  hash?: string;
}
export interface SampleSpace {
  free: number;
  /** null when the device does not say. */
  total: number | null;
}
export interface SampleVerified {
  ok: boolean;
  /** The device's hash of the stored file (hex), null when it does not have the file. For a file checked in ranges: the hash of the first range that differs. */
  device: string | null;
  /** The host's hash of the same bytes (hex). */
  host: string;
  /** The size the device reports and the bytes of the file the host sent. */
  size: number | null;
  sent: number;
  /** Offset of the first range that differs (digikit), else null. */
  mismatchAt: number | null;
  /** What was compared, and a line for the page: "device hash matches", "device content hash matches", or why not. */
  compared: string;
  message: string;
}

export interface RemoveTreeOptions {
  /** Called with (files removed, files to remove) before the first file and after each one. */
  onProgress?: (done: number, total: number) => void;
  /** Asked before each file; true stops the delete there (the folders stay). */
  shouldStop?: () => boolean;
}
export interface RemoveTreeResult {
  removed: number;
  total: number;
  /** `shouldStop` ended the delete before all files were removed. */
  stopped: boolean;
}

export interface SamplesBackend {
  readonly kind: SamplesKind;
  /** The files and folders directly in `folder` ('' is the top). */
  list(folder?: string): Promise<SampleEntry[]>;
  space(): Promise<SampleSpace>;
  /** Whether `mkdir` of an empty folder works (the digikit store needs the runtime's FOLDERS capability). */
  canMkdir(): Promise<boolean>;
  mkdir(folder: string): Promise<void>;
  /** Remove an empty folder. */
  rmdir(folder: string): Promise<void>;
  /** Remove a folder and everything in it, file by file, then the folders. A failure names the file; a cancelled request is rethrown as it is. */
  removeTree(folder: string, o?: RemoveTreeOptions): Promise<RemoveTreeResult>;
  /**
   * Whether `listing` (what `list` returned for the folder the file goes into) holds a file of this name with exactly the bytes `file` will store:
   * the same size and the same content hash. No request: the listing carries the device's hash. A listing cut short only makes this false.
   */
  sameOnDevice(file: Prepared, listing: readonly SampleEntry[]): boolean;
  /**
   * Send the file. `opts.signal` cancels it (a request in flight ends with a `cancelled` error); `opts.exists` is what the caller's listing says about the
   * destination (the bulk path needs it to replace atomically; absent: the backend looks itself).
   */
  put(
    folder: string,
    file: Prepared,
    onProgress?: (sent: number, total: number) => void,
    opts?: PutOptions,
  ): Promise<void>;
  /** 'bulk' while file bytes go over the USB bulk link, 'fallback' after it failed (SysEx or MidiRpc for the rest of the run); absent without a bulk link. */
  readonly bulkState?: 'bulk' | 'fallback';
  /** Compare the stored file with `file` (a device that does not have it, or has other bytes, reports `ok` false). */
  verify(folder: string, file: Prepared): Promise<SampleVerified>;
  remove(folder: string, name: string): Promise<void>;
  /** Whether `rename` works (the digikit store needs the runtime's RENAME capability). */
  canRename(): Promise<boolean>;
  /** Rename or move the file or empty folder `name` of `folder` to `toName` in `toFolder`. Whatever links a sample by its hash still finds it. */
  rename(folder: string, name: string, toFolder: string, toName: string): Promise<void>;
}

export interface PutOptions {
  signal?: AbortSignal;
  exists?: boolean;
  /** The caller made the folder already (the bulk `put` on the +Drive skips its `mkdir`). */
  folderReady?: boolean;
}

/** What the digikit backend needs of the store session (`DkSession`). */
export type StorePort = Pick<
  DkSession,
  'info' | 'list' | 'mkdir' | 'rename' | 'put' | 'delete' | 'verify' | 'capacity'
>;

export interface SamplesOptions {
  /** The transport of the commands: one `DkSession` serves both backends. */
  session: StorePort & RpcPort;
  /** The +Drive content hash (`Sample.hash`); needed by "stock-rpc". */
  hashOf?: (data: Uint8Array) => number;
  /** The USB bulk link, when the device offers its interface (usb-fast): file bytes go over it, everything else stays on `session`. */
  bulk?: BulkSession;
  /** Where the backend reports what it did on its own (a bulk failure that fell back to SysEx). */
  note?: (line: string) => void;
  /** Called when the bulk link failed and the rest of the run falls back: the reason is the logged failure. */
  onFallback?: (reason: string) => void;
}

/** The backend for the profile's `[storage] samples` value. */
export function openSamples(kind: string, o: SamplesOptions): SamplesBackend {
  let base: SamplesBackend;
  let expectedOf: (file: Prepared) => number;
  if (kind === 'stock-rpc') {
    const hashOf = o.hashOf;
    if (!hashOf) throw new DkError({ error: 'stock-rpc samples need the content hash (dksample)', kind: 'usage' });
    base = new RpcSamples(new RpcFiles(o.session), hashOf);
    // the +Drive hashes the file with its 16-byte trailer, which the device adds
    expectedOf = (f) => hashOf(concat(f.bytes, TRAILER)) >>> 0;
  } else if (kind === 'digikit') {
    base = new StoreSamples(o.session);
    expectedOf = (f) => new DataView(fileHash(f.stored).buffer, 0, 4).getUint32(0);
  } else throw new DkError({ error: `unknown samples backend "${kind}" (stock-rpc or digikit)`, kind: 'usage' });
  return o.bulk
    ? new BulkSamples(base, o.bulk, kind === 'digikit', expectedOf, o.note ?? (() => {}), o.onFallback)
    : base;
}

const clean = (folder: string) => folder.replace(/^\/+|\/+$/g, '');
const join = (folder: string, name: string) => {
  if (!name || name.includes('/') || name.includes('\0'))
    throw new DkError({ error: `not a file name: "${name}"`, kind: 'usage' });
  const f = clean(folder);
  return f ? `${f}/${name}` : name;
};
/** A failure of one file names the file; the user's cancel passes as it is (`isCancelled` reads its kind). */
function namedFailure(path: string, e: unknown): unknown {
  if ((e as { kind?: string } | null)?.kind === 'cancelled') return e;
  const msg = e instanceof Error ? e.message : String(e);
  return new Error(`${path}: ${msg}`, { cause: e });
}
/** The top of the device is not a folder to delete. */
function treeRoot(folder: string): string {
  const f = clean(folder);
  if (!f) throw new DkError({ error: 'removeTree needs a folder, not the top', kind: 'usage' });
  return f;
}
const hex8 = (n: number) => (n >>> 0).toString(16).padStart(8, '0');

// ---- the +Drive over MidiRpc ----------------------------------------------------------------------------------------------------

class RpcSamples implements SamplesBackend {
  readonly kind = 'stock-rpc' as const;
  private files: RpcFiles;
  private hashOf: (data: Uint8Array) => number;
  constructor(files: RpcFiles, hashOf: (data: Uint8Array) => number) {
    this.files = files;
    this.hashOf = hashOf;
  }
  async list(folder = '') {
    const rows = await this.files.readDir(`/${clean(folder)}`);
    return rows.map((e) =>
      e.kind === 'D'
        ? { name: e.name, kind: 'folder' as const, size: e.size }
        : { name: e.name, kind: 'file' as const, size: e.size, hash: hex8(e.hash) },
    );
  }
  sameOnDevice(file: Prepared, listing: readonly SampleEntry[]) {
    // the +Drive lists the size without the trailer and `hash | 1` over the file with it (verifyWritten)
    const e = listing.find((x) => x.name === file.name && x.kind === 'file');
    return !!e && e.size === file.bytes.length && e.hash === hex8(this.hashOf(concat(file.bytes, TRAILER)));
  }
  async space() {
    const s = await this.files.space();
    return { free: Number(s.free), total: Number(s.total) };
  }
  async canMkdir() {
    return true;
  }
  mkdir(folder: string) {
    return this.files.mkdir(clean(folder));
  }
  rmdir(folder: string) {
    return this.files.rmdir(clean(folder));
  }
  async removeTree(folder: string, o: RemoveTreeOptions = {}) {
    const root = `/${treeRoot(folder)}`;
    const files: string[] = [];
    const dirs: string[] = []; // a folder after the folders in it
    const walk = async (dir: string) => {
      for (const e of await this.files.readDir(dir)) {
        if (e.kind === 'D') await walk(`${dir}/${e.name}`);
        else files.push(`${dir}/${e.name}`);
      }
      dirs.push(dir);
    };
    await walk(root);
    o.onProgress?.(0, files.length);
    let removed = 0;
    for (const f of files) {
      if (o.shouldStop?.()) return { removed, total: files.length, stopped: true };
      try {
        await this.files.remove(f);
      } catch (e) {
        throw namedFailure(f, e);
      }
      removed++;
      o.onProgress?.(removed, files.length);
    }
    for (const d of dirs)
      try {
        await this.files.rmdir(d);
      } catch (e) {
        throw namedFailure(d, e);
      }
    return { removed, total: files.length, stopped: false };
  }
  async put(folder: string, file: Prepared, onProgress?: (sent: number, total: number) => void, _opts?: PutOptions) {
    await this.files.write(join(folder, file.name), file.bytes, onProgress);
  }
  async verify(folder: string, file: Prepared): Promise<SampleVerified> {
    const v = await verifyWritten(this.files, join(folder, file.name), file.bytes, this.hashOf);
    return {
      ok: v.ok,
      device: v.firmware === null ? null : hex8(v.firmware),
      host: hex8(v.host),
      size: v.size,
      sent: v.sent,
      mismatchAt: null,
      compared: v.compared,
      message: v.ok
        ? 'device content hash matches'
        : v.firmware === null
          ? 'the device does not have the file'
          : 'device content hash differs',
    };
  }
  async remove(folder: string, name: string) {
    return this.files.remove(join(folder, name));
  }
  async canRename() {
    return true;
  }
  rename(folder: string, name: string, toFolder: string, toName: string) {
    return this.files.rename(join(folder, name), join(toFolder, toName));
  }
}

// ---- the digikit store ----------------------------------------------------------------------------------------------------------

/** The folder of the store that holds samples. */
export const STORE_FOLDER = 'samples';
/** VERIFY covers at most this many bytes of a file at a time (one request for a whole large file could outlast the timeout). */
export const VERIFY_PIECE = 1 << 20;

const isFolderEntry = (e: DkEntry) => e.size === 0 && /^0+$/.test(e.hash);

class StoreSamples implements SamplesBackend {
  readonly kind = 'digikit' as const;
  private s: StorePort;
  private caps?: Promise<DkInfoFull>;
  constructor(session: StorePort) {
    this.s = session;
  }
  private info() {
    return (this.caps ??= this.s.info());
  }
  private async folders() {
    return hasCap(await this.info(), 'FOLDERS'); // runtime 0.3.0: LIST by prefix, MKDIR
  }
  /** The store's entries whose path starts with `prefix`: the runtime filters (and pages) when it can, else the host does. */
  private async under(prefix: string): Promise<DkEntry[]> {
    if (await this.folders()) return this.s.list(prefix);
    return (await this.s.list()).filter((e) => e.path.startsWith(prefix));
  }
  private path(folder: string) {
    const f = clean(folder);
    return f ? `${STORE_FOLDER}/${f}` : STORE_FOLDER;
  }
  async list(folder = '') {
    const prefix = `${this.path(folder)}/`;
    const out = new Map<string, SampleEntry>();
    for (const e of await this.under(prefix)) {
      const rest = e.path.slice(prefix.length);
      if (!rest) continue;
      const slash = rest.indexOf('/');
      if (slash >= 0) out.set(rest.slice(0, slash), { name: rest.slice(0, slash), kind: 'folder', size: 0 });
      else if (isFolderEntry(e)) out.set(rest, { name: rest, kind: 'folder', size: 0 });
      else out.set(rest, { name: rest, kind: 'file', size: e.size, hash: e.hash });
    }
    return [...out.values()];
  }
  sameOnDevice(file: Prepared, listing: readonly SampleEntry[]) {
    const e = listing.find((x) => x.name === file.name && x.kind === 'file');
    return !!e && e.size === file.stored.length && e.hash === fileHashHex(file.stored);
  }
  async space() {
    return storeSpace(this.s);
  }
  canMkdir() {
    return this.folders();
  }
  async mkdir(folder: string) {
    if (!(await this.folders())) throw new DkError({ error: 'this runtime has no folder entries', kind: 'usage' });
    await this.s.mkdir(this.path(folder));
  }
  async rmdir(folder: string) {
    // a folder entry is an entry of the store; a folder with files in it is only implied by their paths (nothing to delete)
    await this.s.delete(this.path(folder));
  }
  async removeTree(folder: string, o: RemoveTreeOptions = {}) {
    const top = this.path(treeRoot(folder));
    const prefix = `${top}/`; // the slash keeps `Foo` from matching `Foobar/...`
    const inside = (await this.under(prefix)).filter((e) => e.path.startsWith(prefix));
    const files = inside.filter((e) => !isFolderEntry(e)).map((e) => e.path);
    // folder entries inside the folder, the deepest first
    const dirs = inside
      .filter(isFolderEntry)
      .map((e) => e.path)
      .sort((a, b) => b.split('/').length - a.split('/').length);
    // a folder with files is only implied by their paths, so it vanishes with its last file: "not in the store" means gone
    const del = async (path: string) => {
      try {
        await this.s.delete(path);
      } catch (e) {
        if ((e as { status?: string } | null)?.status !== 'E_NOFILE') throw namedFailure(path, e);
      }
    };
    o.onProgress?.(0, files.length);
    let removed = 0;
    for (const f of files) {
      if (o.shouldStop?.()) return { removed, total: files.length, stopped: true };
      await del(f);
      removed++;
      o.onProgress?.(removed, files.length);
    }
    for (const d of dirs) await del(d);
    await del(top);
    return { removed, total: files.length, stopped: false };
  }
  async put(folder: string, file: Prepared, onProgress?: (sent: number, total: number) => void, _opts?: PutOptions) {
    return this.s.put(this.path(join(folder, file.name)), file.stored, onProgress);
  }
  async verify(folder: string, file: Prepared): Promise<SampleVerified> {
    const path = this.path(join(folder, file.name));
    const want = file.stored;
    const host = fileHashHex(want);
    const entry = (await this.under(path)).find((e) => e.path === path && !isFolderEntry(e));
    const base = {
      host,
      sent: want.length,
      mismatchAt: null,
      compared: 'the device hash of the stored file, read from the card (VERIFY), with the hash of the bytes sent',
    };
    if (!entry) return { ...base, ok: false, device: null, size: null, message: 'the device does not have the file' };
    // one VERIFY for a small file, ranges of VERIFY_PIECE for a large one (all the ranges equal: the whole file is equal)
    let device = host;
    let mismatchAt: number | null = null;
    for (let off = 0; off < Math.max(1, want.length); off += VERIFY_PIECE) {
      const part = want.subarray(off, Math.min(want.length, off + VERIFY_PIECE));
      const whole = want.length <= VERIFY_PIECE;
      const got = whole ? await this.s.verify(path) : await this.s.verify(path, off, part.length);
      if (got !== (whole ? host : fileHashHex(part))) {
        device = got;
        mismatchAt = off;
        break;
      }
    }
    const ok = mismatchAt === null && entry.size === want.length;
    return {
      ...base,
      ok,
      device,
      size: entry.size,
      mismatchAt,
      message: ok
        ? 'device hash matches'
        : mismatchAt === null
          ? 'the device file has another size'
          : 'device hash differs',
    };
  }
  async remove(folder: string, name: string) {
    return this.s.delete(this.path(join(folder, name)));
  }
  async canRename() {
    return hasCap(await this.info(), 'RENAME'); // runtime 0.5.7
  }
  async rename(folder: string, name: string, toFolder: string, toName: string) {
    if (!(await this.canRename())) throw new DkError({ error: 'this runtime cannot rename', kind: 'usage' });
    await this.s.rename(this.path(join(folder, name)), this.path(join(toFolder, toName)));
  }
}

// ---- file bytes over the USB bulk link ----------------------------------------------------------------------------------------------

/** `DK_FILES_SAMPLES` (<dk/files.h>): the `samples/` folder of the store on the Digitone II, the +Drive on the Digitakt II. */
export const BULK_ROOT_SAMPLES = 1;

/**
 * The backend of the device, with `put` over the bulk link (usb-fast). Listing, folders, delete, verify and the space stay on the base backend (SysEx or
 * MidiRpc). A file that exists is replaced atomically by the device (REPLACE flag, never delete then upload). The device checks the file
 * hash at the end of the transfer and the session compares what it returns, so a `put` that returns is verified; `verify` stays available.
 */
class BulkSamples implements SamplesBackend {
  readonly kind: SamplesKind;
  private base: SamplesBackend;
  private bulk: BulkSession;
  private store: boolean;
  private expectedOf: (file: Prepared) => number;
  private note: (line: string) => void;
  private failed = false;
  private onFallback?: (reason: string) => void;
  constructor(
    base: SamplesBackend,
    bulk: BulkSession,
    store: boolean,
    expectedOf: (file: Prepared) => number,
    note: (line: string) => void,
    onFallback?: (reason: string) => void,
  ) {
    this.onFallback = onFallback;
    this.base = base;
    this.kind = base.kind;
    this.bulk = bulk;
    this.store = store;
    this.expectedOf = expectedOf;
    this.note = note;
  }
  get bulkState() {
    return this.failed ? ('fallback' as const) : ('bulk' as const);
  }
  list(folder?: string) {
    return this.base.list(folder);
  }
  space() {
    return this.base.space();
  }
  canMkdir() {
    return this.base.canMkdir();
  }
  mkdir(folder: string) {
    return this.base.mkdir(folder);
  }
  canRename() {
    return this.base.canRename();
  }
  rename(folder: string, name: string, toFolder: string, toName: string) {
    return this.base.rename(folder, name, toFolder, toName);
  }
  rmdir(folder: string) {
    return this.base.rmdir(folder);
  }
  removeTree(folder: string, o?: RemoveTreeOptions) {
    return this.base.removeTree(folder, o);
  }
  sameOnDevice(file: Prepared, listing: readonly SampleEntry[]) {
    return this.base.sameOnDevice(file, listing);
  }
  verify(folder: string, file: Prepared) {
    return this.base.verify(folder, file);
  }
  remove(folder: string, name: string) {
    return this.base.remove(folder, name);
  }
  async put(folder: string, file: Prepared, onProgress?: (sent: number, total: number) => void, opts: PutOptions = {}) {
    if (this.failed) return this.base.put(folder, file, onProgress, opts);
    const path = join(folder, file.name);
    // the +Drive needs the folder; the store's folders follow from the paths
    if (!this.store && clean(folder) && !opts.folderReady) await this.base.mkdir(folder);
    const bytes = this.store ? file.stored : file.bytes;
    // the caller's listing says whether the file exists: then the device replaces it atomically (a failed upload keeps the old file). Without a hint
    // the backend looks once. A file that appeared after the listing is refused (E_EXISTS) and reported.
    const replace =
      opts.exists ?? (await this.base.list(folder)).some((e) => e.name === file.name && e.kind === 'file');
    try {
      await this.bulk.put(BULK_ROOT_SAMPLES, path, bytes, {
        expected: this.expectedOf(file),
        onProgress,
        replace,
        signal: opts.signal,
      });
    } catch (e) {
      // the device answered (E_FULL, E_EXISTS, E_HASH ...) or the user cancelled: those are the result. Anything else is the link: SysEx for the rest of the run
      if (e instanceof BulkDeviceError || (e as { kind?: string } | null)?.kind === 'cancelled' || opts.signal?.aborted)
        throw e;
      this.failed = true;
      const reason = e instanceof Error ? e.message : String(e);
      this.onFallback?.(reason);
      this.note(
        `USB bulk link failed (${reason}); the rest of this run goes over ${this.base.kind === 'digikit' ? 'SysEx' : 'MidiRpc'}`,
      );
      return this.base.put(folder, file, onProgress, opts);
    }
  }
}
