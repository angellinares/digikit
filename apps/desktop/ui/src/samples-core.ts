// The pure logic of the Samples mode of the Transfer page (no DOM, no Tauri): folder paths and breadcrumbs, the rows of the local folder and of
// the device's folder with the state of each upload, and the upload run over a samples backend (packages/dk-session/src/samples.ts).
// Written without TS-only runtime syntax so node can import it (apps/web/tests/samples-page.test.mjs).
import { errorLine, isCancelled } from '../../../web/src/modules/backend.ts';
import type { Prepared } from '@digi/dk-session/dksample';
import type { SampleEntry, SamplesBackend } from '@digi/dk-session/samples';
import { fmtRate } from '../../../web/src/transfer/rate-core.ts';
import type { BarInfo, RowStatus, RowTag } from '../../../web/src/transfer/transfer-core.ts';

// ---- folders ----------------------------------------------------------------------------------------------------------------------

export const joinPath = (folder: string, name: string): string => (folder ? `${folder}/${name}` : name);
export const parentOf = (folder: string): string => folder.split('/').slice(0, -1).join('/');

export interface Crumb {
  name: string;
  /** The folder this crumb opens ('' is the top). */
  path: string;
}
/** The breadcrumb of a folder: the top, then one crumb per level. */
export function crumbs(folder: string, top = 'Samples'): Crumb[] {
  const out: Crumb[] = [{ name: top, path: '' }];
  let path = '';
  for (const part of folder.split('/').filter(Boolean)) {
    path = joinPath(path, part);
    out.push({ name: part, path });
  }
  return out;
}

/** Why `name` cannot be a new folder here, or '' when it can. */
export function folderNameError(name: string, entries: SampleEntry[]): string {
  if (!name.trim()) return 'Give the folder a name.';
  if (name !== name.trim() || name === '.' || name === '..' || /[/\\\0]/.test(name)) return 'Not a folder name.';
  if (entries.some((e) => e.name === name)) return `${name} exists already.`;
  return '';
}

// ---- the local folder ("On your computer") ---------------------------------------------------------------------------------------

/** What the native `local_list` returns for one level of the chosen folder. */
export interface LocalEntry {
  name: string;
  kind: 'file' | 'folder';
  size: number;
}
/** A file added by "Add files…" or a drop: kept in the list even when it is outside the chosen folder. */
export interface Added {
  name: string;
  size: number;
}
export interface LocalRow {
  /** The key of ticks and kinds: the path inside the chosen folder, or `+name` for an added file. */
  key: string;
  name: string;
  /** The path inside the chosen folder (a folder row opens it); empty for an added file. */
  path: string;
  kind: 'file' | 'folder';
  size: number;
  added: boolean;
  /** What the upload will do with a file, once its first bytes were read. */
  sample?: Prepared['kind'];
}

/** The last part of a path (`/Users/em/samples` is `samples`; either separator). */
export const baseName = (path: string): string => path.split(/[\\/]/).filter(Boolean).pop() ?? path;

/** The breadcrumb of the local folder: the chosen folder by its name, then one crumb per level. */
export const localCrumbs = (root: string, folder: string): Crumb[] => crumbs(folder, baseName(root));

/** The rows of the local list: the folders, then the files of the open level by name, then the added files in the order they came. */
export function localRows(
  folder: string,
  entries: LocalEntry[],
  added: Added[],
  kinds: Record<string, Prepared['kind']>,
): LocalRow[] {
  const rows: LocalRow[] = entries.map((e) => {
    const path = joinPath(folder, e.name);
    return { key: path, name: e.name, path, kind: e.kind, size: e.size, added: false, sample: kinds[path] };
  });
  rows.sort((a, b) => (a.kind === b.kind ? a.name.localeCompare(b.name) : a.kind === 'folder' ? -1 : 1));
  for (const a of added)
    rows.push({
      key: `+${a.name}`,
      name: a.name,
      path: '',
      kind: 'file',
      size: a.size,
      added: true,
      sample: kinds[`+${a.name}`],
    });
  return rows;
}

/** The tags of a local row: what the upload will do with a file (nothing is said before its first bytes were read, and nothing of a folder),
 * and "added" for a file added by hand. */
export function localRowTags(r: LocalRow): RowTag[] {
  const out: RowTag[] = [];
  const kind = r.kind === 'file' && r.sample ? kindTag(r.sample) : undefined;
  if (kind) out.push(kind);
  if (r.added) out.push({ text: 'added', title: 'Added by hand, from outside the folder', tone: 'dim' });
  return out;
}

/** The selected rows (files and folders), in list order (a selection of rows that are gone is dropped). */
export const selectedRows = (rows: LocalRow[], selected: ReadonlySet<string>): LocalRow[] =>
  rows.filter((r) => selected.has(r.key));

// ---- uploads ----------------------------------------------------------------------------------------------------------------------

/** One file of an upload run, from "waiting" to its result. A file that could not be prepared (a broken WAV) is `failed` with kind `none`.
 * `same`: the device had the file with the same bytes already, so nothing was sent (and nothing was read back). `cancelled`: the user stopped the run before the file was verified (the message says how far it got). */
export interface Upload {
  folder: string;
  /** The name on the device (a WAV loses its extension). */
  name: string;
  /** The name of the local file. */
  source: string;
  kind: Prepared['kind'] | 'none';
  state: 'waiting' | 'uploading' | 'verifying' | 'ok' | 'same' | 'failed' | 'cancelled';
  sent: number;
  bytes: number;
  /** The compared message of the verify (the tooltip of the chip), or the error. */
  message: string;
}
/** Per device path (`folder/name`). */
export type Uploads = Record<string, Upload>;

export type UploadEvent =
  | { event: 'queued'; folder: string; name: string; bytes: number }
  | {
      event: 'prepared';
      folder: string;
      name: string;
      source: string;
      kind: Prepared['kind'];
      bytes: number;
      /** The name the file was queued under (the local name; a WAV loses its extension on the device). */
      queued?: string;
    }
  | { event: 'progress'; folder: string; name: string; sent: number; bytes: number }
  | { event: 'verifying'; folder: string; name: string }
  | { event: 'verified'; folder: string; name: string; ok: boolean; message: string }
  | { event: 'same'; folder: string; name: string }
  /** One line for the Log that belongs to no file (a folder listing that failed). */
  | { event: 'note'; message: string }
  | { event: 'failed'; folder: string; name: string; source?: string; message: string }
  | { event: 'cancelled'; folder: string; name: string; message: string };

/** The uploads after one more event (a new object). A queued file has the size of the local file and the local name; `prepared` gives it the
 * name and size it has on the device, in the same place of the list. */
export function applyUpload(u: Uploads, e: UploadEvent): Uploads {
  if (e.event === 'note') return u;
  const key = joinPath(e.folder, e.name);
  const base: Upload = u[key] ?? {
    folder: e.folder,
    name: e.name,
    source: e.event === 'failed' ? (e.source ?? e.name) : e.name,
    kind: 'none',
    state: 'waiting',
    sent: 0,
    bytes: 0,
    message: '',
  };
  switch (e.event) {
    case 'queued':
      return { ...u, [key]: { ...base, bytes: e.bytes, state: 'waiting', sent: 0, message: '' } };
    case 'prepared': {
      const next: Upload = { ...base, source: e.source, kind: e.kind, bytes: e.bytes, state: 'waiting' };
      const was = e.queued !== undefined && e.queued !== e.name ? joinPath(e.folder, e.queued) : '';
      if (!was || !(was in u)) return { ...u, [key]: next };
      // the queued entry becomes this one without moving
      return Object.fromEntries(Object.entries(u).map(([k, v]) => (k === was ? [key, next] : [k, v])));
    }
    case 'progress':
      return { ...u, [key]: { ...base, state: 'uploading', sent: e.sent, bytes: e.bytes } };
    case 'verifying':
      return { ...u, [key]: { ...base, state: 'verifying', sent: base.bytes } };
    case 'verified':
      return { ...u, [key]: { ...base, state: e.ok ? 'ok' : 'failed', message: e.message } };
    case 'same':
      return { ...u, [key]: { ...base, state: 'same', sent: base.bytes, message: SAME_MESSAGE } };
    case 'failed':
      return { ...u, [key]: { ...base, state: 'failed', message: e.message } };
    case 'cancelled':
      return { ...u, [key]: { ...base, state: 'cancelled', message: e.message } };
  }
}

/** The tooltip of a file that was not sent: what was compared, and how to get a fresh read-back. */
export const SAME_MESSAGE =
  'The device has this file with the same size and content hash: not sent, not read back. To read it back from the card, delete it and upload it again.';

/** What a local file will become on upload, as a small tag: a WAV is converted, a native sample goes as it is, anything else is not a sample. */
export function kindTag(kind: Upload['kind']): RowTag | undefined {
  switch (kind) {
    case 'converted':
      return { text: 'WAV→', title: 'Converted to the native sample file on upload', tone: 'dim' };
    case 'native':
      return { text: 'native', title: 'A native sample file: uploaded as it is', tone: 'dim' };
    case 'unchanged':
      return { text: 'other', title: "Won't play as a sample", tone: 'dim' };
    default:
      return undefined;
  }
}

/** What a row of the device list shows for its upload: the shape of the Modules page (`RowStatus`), plus waiting and the tooltip. */
export type SampleStatus = (RowStatus | { kind: 'waiting' } | { kind: 'same' } | { kind: 'cancelled' }) & {
  message: string;
};
export function uploadStatus(u: Upload | undefined): SampleStatus | undefined {
  if (!u) return undefined;
  const message = u.message;
  switch (u.state) {
    case 'waiting':
      return { kind: 'waiting', message };
    case 'uploading':
      return u.sent < u.bytes
        ? { kind: 'uploading', sent: u.sent, bytes: u.bytes, message }
        : { kind: 'verifying', message };
    case 'verifying':
      return { kind: 'verifying', message };
    case 'ok':
      return { kind: 'ok', message };
    case 'same':
      return { kind: 'same', message };
    case 'failed':
      return { kind: 'failed', message };
    case 'cancelled':
      return { kind: 'cancelled', message };
  }
}

/** "87 of 357 uploaded, 270 not sent": what a stopped run did; files that were on the device already are named, not counted as not sent. */
export const cancelledCounts = (ok: number, n: number, same = 0): string =>
  `${ok} of ${n} uploaded, ${same ? `${same} already on the device, ` : ''}${n - ok - same} not sent`;

/** The bar of an upload run: the uploads that this run touched (`base` is what `uploads` held when the run started; an entry that is the very
 * same object was not touched). `running`: the run is still going, so the message names the file. `average`: bytes per second of the whole
 * run, for the end text. A cancelled run is not red, and the bar shows the bytes that went, never more. No entry: no bar (undefined). */
export function samplesBar(uploads: Uploads, base: Uploads, running: boolean, average?: number): BarInfo | undefined {
  const mine = Object.entries(uploads).filter(([key, u]) => base[key] !== u);
  if (!mine.length) return undefined;
  // `sent` and `total` are the bytes this run really sends: they give the speed and the time left, so a file that was on the device already
  // counts in neither (the fraction counts it as done at once)
  let sent = 0;
  let total = 0;
  let skipped = 0;
  let ok = 0;
  let same = 0;
  let failed = 0;
  let cancelled = 0;
  for (const [, u] of mine) {
    if (u.state === 'same') {
      same++;
      skipped += u.bytes;
      continue;
    }
    total += u.bytes;
    if (u.state === 'ok' || u.state === 'verifying') sent += u.bytes;
    else if (u.state !== 'waiting') sent += u.sent;
    if (u.state === 'ok') ok++;
    else if (u.state === 'failed') failed++;
    else if (u.state === 'cancelled') cancelled++;
  }
  const n = mine.length;
  const current = mine.find(([, u]) => u.state === 'uploading' || u.state === 'verifying')?.[1];
  const plural = (k: number) => `${k} file${k === 1 ? '' : 's'}`;
  let message: string;
  if (running)
    message = current
      ? `${current.state === 'verifying' ? 'Verifying' : 'Uploading'} ${current.name} (${ok + same + failed + cancelled + 1} of ${n})`
      : 'Preparing…';
  else if (failed)
    message = `${ok} of ${n} uploaded, ${same ? `${same} already on the device, ` : ''}${failed} failed.`;
  else if (cancelled) message = `Cancelled: ${cancelledCounts(ok, n, same)}.`;
  else if (same === n)
    message = n === 1 ? 'The file is already on the device.' : `All ${n} files are already on the device.`;
  else if (ok === 0 && same === 0) message = 'Nothing was uploaded.';
  else if (ok + same < n) message = `${ok} of ${n} uploaded${same ? `, ${same} already on the device` : ''}.`;
  else if (same) message = `Uploaded ${plural(ok)}, ${same} already on the device.`;
  else message = `Uploaded ${plural(ok)}, verified.`;
  if (!running && average !== undefined && sent > 0) message += ` · ${fmtRate(average)} average`;
  // a run that ended with files not finished (stopped between the queue and the end) is not a success either
  const unfinished = !running && !failed && !cancelled && ok + same < n;
  const all = total + skipped;
  const fraction = all ? Math.min(1, (sent + skipped) / all) : running ? 0 : 1;
  return {
    fraction: running || failed || cancelled || unfinished ? fraction : 1,
    failed: failed > 0 || unfinished,
    message,
    sent,
    total,
  };
}

/** The uploads that a run touched (`base` is what `uploads` held when it started). */
const touched = (uploads: Uploads, base: Uploads): Upload[] =>
  Object.entries(uploads)
    .filter(([key, u]) => base[key] !== u)
    .map(([, u]) => u);

/** One line per file of a run, for the Log: `a: verified`, `b: not sent: ...`, `c: failed: <message>`, `d: cancelled while sending`. */
export function uploadLines(uploads: Uploads, base: Uploads): string[] {
  return touched(uploads, base).map((u) => {
    const why = u.message.split('\n')[0] ?? '';
    if (u.state === 'ok') return `${u.name}: verified`;
    if (u.state === 'same') return `${u.name}: already on the device`;
    if (u.state === 'cancelled') return `${u.name}: ${why}`;
    if (u.state === 'failed') return why.startsWith('not sent:') ? `${u.name}: ${why}` : `${u.name}: failed: ${why}`;
    return `${u.name}: not finished`;
  });
}

/** The error of a run in which a file failed or the run ended early, or undefined when all were verified or the user cancelled:
 * `Uploading failed: <first failed message> (1 of 3 sent, 2 not sent)`. A cancelled run is `uploadCancelled`. */
export function uploadFailure(uploads: Uploads, base: Uploads): string | undefined {
  const mine = touched(uploads, base);
  const ok = mine.filter((u) => u.state === 'ok' || u.state === 'same').length;
  const first = mine.find((u) => u.state === 'failed');
  if (!mine.length || (!first && mine.every((u) => u.state === 'ok' || u.state === 'same' || u.state === 'cancelled')))
    return undefined;
  const why = first ? (first.message.split('\n')[0] ?? '') : 'the run ended before every file was sent';
  return `Uploading failed: ${why} (${ok} of ${mine.length} sent, ${mine.length - ok} not sent)`;
}

/** What the run did when the user stopped it and no file failed (`87 of 357 uploaded, 270 not sent`), else undefined. */
export function uploadCancelled(uploads: Uploads, base: Uploads): string | undefined {
  const mine = touched(uploads, base);
  if (uploadFailure(uploads, base) || !mine.some((u) => u.state === 'cancelled')) return undefined;
  return cancelledCounts(
    mine.filter((u) => u.state === 'ok').length,
    mine.length,
    mine.filter((u) => u.state === 'same').length,
  );
}

/** The uploads without the given device paths and, for a folder, everything under it (`Foo/` does not match `Foobar/`): their files were deleted. */
export function dropUploads(u: Uploads, paths: string[]): Uploads {
  const out = { ...u };
  for (const key of Object.keys(out)) if (paths.some((p) => key === p || key.startsWith(`${p}/`))) delete out[key];
  return out;
}

/** The line while a folder is deleted: `Deleting 20 of 357`, with the folder's name when several rows go. */
export const deletingLine = (done: number, total: number, folder?: string): string =>
  `Deleting ${folder ? `${folder}: ` : ''}${done} of ${total}`;

/** What a stopped or failed delete says: how many files went and what remains. */
export const deletePartLine = (done: number, total: number): string =>
  `${done} of ${total} file${total === 1 ? '' : 's'} deleted, ${total - done} left`;

/** The text of the armed Delete button when a folder is ticked: its contents go too. */
export function confirmDeleteLabel(rows: { kind: 'file' | 'folder' }[]): string | undefined {
  const folders = rows.filter((r) => r.kind === 'folder').length;
  if (!folders) return undefined;
  return rows.length === 1
    ? 'Delete this folder and everything in it?'
    : `Delete ${rows.length} items, folders with everything in them?`;
}

// ---- the device's folder ----------------------------------------------------------------------------------------------------------

export interface SampleRow {
  name: string;
  /** The path from the top: the key of ticks and uploads. */
  path: string;
  kind: 'file' | 'folder';
  size: number;
  /** A file of an upload that is not in the listing (yet). */
  pending: boolean;
  status?: SampleStatus;
}

/** The state of a folder that an upload run writes into, from the uploads inside it: a bar while anything is going, else failed when one
 * failed, else verified. Undefined when none of its uploads were touched. */
export function folderStatus(uploads: Uploads, folder: string): SampleStatus | undefined {
  const inside = Object.values(uploads).filter((u) => u.folder === folder || u.folder.startsWith(`${folder}/`));
  if (!inside.length) return undefined;
  const open = inside.filter((u) => !['ok', 'same', 'failed', 'cancelled'].includes(u.state));
  if (open.length) {
    const bytes = inside.reduce((a, u) => a + u.bytes, 0);
    const sent = inside.reduce(
      (a, u) => a + (u.state === 'ok' || u.state === 'same' || u.state === 'verifying' ? u.bytes : u.sent),
      0,
    );
    return open.every((u) => u.state === 'waiting')
      ? { kind: 'waiting', message: '' }
      : { kind: 'uploading', sent, bytes, message: '' };
  }
  const bad = inside.filter((u) => u.state === 'failed');
  if (bad.length) return { kind: 'failed', message: `${bad.length} of ${inside.length} files failed` };
  const stopped = inside.filter((u) => u.state === 'cancelled');
  if (stopped.length) return { kind: 'cancelled', message: `${stopped.length} of ${inside.length} files not sent` };
  const same = inside.filter((u) => u.state === 'same').length;
  const files = (k: number) => `${k} file${k === 1 ? '' : 's'}`;
  if (same === inside.length) return { kind: 'same', message: `${files(same)} already on the device` };
  return {
    kind: 'ok',
    message: same
      ? `${files(inside.length - same)} verified, ${same} already on the device`
      : `${files(inside.length)} verified`,
  };
}

/** The rows of `folder`: folders first, then files, each by name. The files of an upload run that the listing does not hold are rows too, and
 * so is a folder that the run is filling (made on the device, not listed yet); a folder row shows the state of the uploads inside it. */
export function sampleRows(folder: string, entries: SampleEntry[], uploads: Uploads): SampleRow[] {
  const rows: SampleRow[] = entries.map((e) => {
    const path = joinPath(folder, e.name);
    return {
      name: e.name,
      path,
      kind: e.kind,
      size: e.size,
      pending: false,
      status: e.kind === 'file' ? uploadStatus(uploads[path]) : folderStatus(uploads, path),
    };
  });
  const have = new Set(rows.map((r) => r.path));
  const prefix = folder ? `${folder}/` : '';
  for (const [path, u] of Object.entries(uploads)) {
    if (u.folder === folder) {
      if (!have.has(path))
        rows.push({ name: u.name, path, kind: 'file', size: u.bytes, pending: true, status: uploadStatus(u) });
    } else if (u.folder.startsWith(prefix)) {
      const name = u.folder.slice(prefix.length).split('/')[0] as string;
      const sub = joinPath(folder, name);
      if (!have.has(sub)) {
        have.add(sub);
        rows.push({ name, path: sub, kind: 'folder', size: 0, pending: true, status: folderStatus(uploads, sub) });
      }
    }
  }
  return rows.sort((a, b) => (a.kind === b.kind ? a.name.localeCompare(b.name) : a.kind === 'folder' ? -1 : 1));
}

/** The ticked rows of the folder, in list order (ticks of rows that are gone are dropped). Pending rows are not on the device. */
export const deleteRows = (rows: SampleRow[], ticked: Set<string>): SampleRow[] =>
  rows.filter((r) => !r.pending && ticked.has(r.path));

// ---- the plan of an upload --------------------------------------------------------------------------------------------------------

/** One file of an upload plan: where it goes on the device, and where it comes from (`rel` inside the chosen folder, or a file added by hand). */
export interface PlannedFile {
  /** The device folder it goes into. */
  folder: string;
  /** Its local name (a WAV loses its extension on the device). */
  name: string;
  size: number;
  /** The path inside the chosen folder; empty for an added file (found by `name`). */
  rel: string;
  added: boolean;
}
export interface UploadPlan {
  /** Device folders to make, parents before the folders inside them; each once. */
  mkdirs: string[];
  files: PlannedFile[];
}

/** A folder that nests deeper than this, or a plan of more files than `MAX_PLAN_FILES`, is refused (a link loop inside the folder would
 * otherwise never end). */
export const MAX_PLAN_DEPTH = 24;
export const MAX_PLAN_FILES = 20000;

/**
 * What uploading the selected rows into the device folder `target` means. A file goes into `target`; a folder is made inside it and its
 * contents follow, recursively, with the structure kept (`list` reads one level of the chosen folder). Folders are made before what they hold
 * and parents before children; a folder is listed folders first, then files by name, the order of the list. An empty folder is made too.
 */
export async function planUpload(
  rows: readonly Pick<LocalRow, 'kind' | 'name' | 'path' | 'size' | 'added'>[],
  target: string,
  list: (rel: string) => Promise<LocalEntry[]>,
): Promise<UploadPlan> {
  const mkdirs: string[] = [];
  const files: PlannedFile[] = [];
  const walk = async (rel: string, dest: string, depth: number) => {
    if (depth > MAX_PLAN_DEPTH)
      throw new Error(`${rel}: folders nest more than ${MAX_PLAN_DEPTH} levels deep (a link loop?)`);
    const entries = [...(await list(rel))].sort((a, b) =>
      a.kind === b.kind ? a.name.localeCompare(b.name) : a.kind === 'folder' ? -1 : 1,
    );
    for (const e of entries) {
      if (e.kind === 'folder') {
        const sub = joinPath(dest, e.name);
        mkdirs.push(sub);
        await walk(joinPath(rel, e.name), sub, depth + 1);
      } else {
        files.push({ folder: dest, name: e.name, size: e.size, rel: joinPath(rel, e.name), added: false });
        if (files.length > MAX_PLAN_FILES)
          throw new Error(`more than ${MAX_PLAN_FILES} files: choose a smaller folder`);
      }
    }
  };
  for (const r of rows) {
    if (r.kind === 'file') {
      files.push({ folder: target, name: r.name, size: r.size, rel: r.path, added: r.added });
    } else {
      const dest = joinPath(target, r.name);
      mkdirs.push(dest);
      await walk(r.path, dest, 1);
    }
  }
  return { mkdirs: [...new Set(mkdirs)], files };
}

/** What the page says about folders dragged in from outside the app (only their names reach the page, not what is in them). */
export const foldersFromOutside = (n: number): string =>
  `${n === 1 ? 'A folder' : `${n} folders`} dragged in from outside cannot be uploaded: choose it with "Choose folder…", then drag it from the list.`;

/** What the page says when the device's storage cannot make folders and the selection holds a folder. */
export const noFoldersMessage = (device: string): string =>
  `The ${device} storage has no folders, so a folder cannot be uploaded with its structure. Select the files and upload them one by one.`;

// ---- an upload run ----------------------------------------------------------------------------------------------------------------

/** What `prepareUpload` of dksample.ts is, with the wasm bound. */
export type Prepare = (name: string, bytes: Uint8Array) => Prepared;

/** One file to send: its device folder, its local name and size, and how to read it (read only when its turn comes). */
export interface UploadItem {
  folder: string;
  name: string;
  size: number;
  read: () => Promise<Uint8Array>;
}

const under = (folder: string, parent: string) => folder === parent || folder.startsWith(`${parent}/`);

/**
 * Make the folders (those that the device does not have yet), then put the files: each is read, prepared (a WAV converted), sent with its
 * progress, then verified against what was sent. Every file is queued first, so the global bar knows the whole run. A file that cannot be read
 * or prepared fails alone; a folder that cannot be made fails the files inside it; a failed send stops the run (the device or the link is gone)
 * and the files after it fail as not sent. `stop` says the user cancelled: no later file starts, and a cancelled request ends the run the
 * same way. A file that the cancel cut off, and every file not started, ends `cancelled` (not failed); a file verified before stays `ok`.
 * Every step is an event; nothing throws.
 *
 * A file that the device holds already with the same bytes is not sent (`same`, no verify: the device's hash was read from its listing). The
 * backend lists each folder once, the first time the run needs it (a folder this run made is empty, so it is not listed), and `sameOnDevice`
 * compares the file as `prepare` made it with that listing. A listing that fails is logged once (`note`) and the files go as if nothing was there.
 */
export async function uploadItems(
  backend: Pick<SamplesBackend, 'put' | 'verify'> & Partial<Pick<SamplesBackend, 'list' | 'mkdir' | 'sameOnDevice'>>,
  prepare: Prepare,
  mkdirs: readonly string[],
  items: readonly UploadItem[],
  emit: (e: UploadEvent) => void,
  stop: () => boolean = () => false,
): Promise<void> {
  for (const it of items) emit({ event: 'queued', folder: it.folder, name: it.name, bytes: it.size });
  const lost = new Map<string, string>();
  let cancelled = false;
  // what the device held in each folder when the run first looked (null: the listing failed)
  const listings = new Map<string, SampleEntry[] | null>();
  const listingOf = async (folder: string): Promise<SampleEntry[] | null> => {
    if (listings.has(folder)) return listings.get(folder) ?? null;
    let got: SampleEntry[] | null = null;
    try {
      got = (await backend.list?.(folder)) ?? null;
    } catch (e) {
      if (isCancelled(e)) throw e;
      emit({
        event: 'note',
        message: `${folder || 'the top folder'}: could not list it, so every file goes: ${errorLine(e)}`,
      });
    }
    listings.set(folder, got);
    return got;
  };
  for (const dir of mkdirs) {
    if (stop()) break;
    const parent = parentOf(dir);
    const inLost = [...lost.keys()].find((l) => under(dir, l));
    if (inLost) {
      lost.set(dir, lost.get(inLost) as string);
      continue;
    }
    try {
      const have = listings.get(parent) ?? (await backend.list?.(parent));
      if (have && !listings.has(parent)) listings.set(parent, have);
      if (!have?.some((e) => e.name === dir.split('/').pop() && e.kind === 'folder')) {
        await backend.mkdir?.(dir);
        listings.set(dir, []); // made by this run: nothing in it to skip
      }
    } catch (e) {
      if (isCancelled(e)) {
        cancelled = true;
        break;
      }
      lost.set(dir, `the folder ${dir} could not be made: ${errorLine(e)}`);
    }
  }
  let stopped = '';
  for (const it of items) {
    const { folder } = it;
    if (cancelled || stop()) {
      cancelled = true;
      emit({ event: 'cancelled', folder, name: it.name, message: 'not sent: cancelled' });
      continue;
    }
    const why = [...lost.entries()].find(([dir]) => under(folder, dir))?.[1];
    if (why || stopped) {
      emit({ event: 'failed', folder, name: it.name, source: it.name, message: `not sent: ${why ?? stopped}` });
      continue;
    }
    let p: Prepared;
    try {
      p = prepare(it.name, await it.read());
      emit({
        event: 'prepared',
        folder,
        name: p.name,
        source: it.name,
        kind: p.kind,
        bytes: p.stored.length,
        queued: it.name,
      });
    } catch (e) {
      emit({ event: 'failed', folder, name: it.name, source: it.name, message: errorLine(e) });
      continue;
    }
    // the cancel came while the file was read: nothing of it was sent
    if (stop()) {
      cancelled = true;
      emit({ event: 'cancelled', folder, name: p.name, message: 'not sent: cancelled' });
      continue;
    }
    if (backend.sameOnDevice && backend.list) {
      let have: SampleEntry[] | null;
      try {
        have = await listingOf(folder);
      } catch {
        cancelled = true;
        emit({ event: 'cancelled', folder, name: p.name, message: 'not sent: cancelled' });
        continue;
      }
      if (have && backend.sameOnDevice(p, have)) {
        emit({ event: 'same', folder, name: p.name });
        continue;
      }
    }
    // what the listing says about the destination (the bulk path replaces an existing file atomically); the user's stop reaches a send in flight
    const exists = listings.get(folder)?.some((e) => e.name === p.name && e.kind === 'file');
    const abort = new AbortController();
    const watch = setInterval(() => {
      if (stop()) abort.abort();
    }, 50);
    try {
      await backend.put(folder, p, (sent, bytes) => emit({ event: 'progress', folder, name: p.name, sent, bytes }), {
        signal: abort.signal,
        exists,
      });
    } catch (e) {
      if (isCancelled(e)) {
        cancelled = true;
        emit({ event: 'cancelled', folder, name: p.name, message: 'cancelled while sending' });
        continue;
      }
      emit({ event: 'failed', folder, name: p.name, message: errorLine(e) });
      stopped = `the upload of ${p.name} failed`;
      continue;
    } finally {
      clearInterval(watch);
    }
    emit({ event: 'verifying', folder, name: p.name });
    try {
      const v = await backend.verify(folder, p);
      emit({ event: 'verified', folder, name: p.name, ok: v.ok, message: `${v.message}\n${v.compared}` });
    } catch (e) {
      if (isCancelled(e)) {
        cancelled = true;
        emit({ event: 'cancelled', folder, name: p.name, message: 'sent, not verified: cancelled' });
        continue;
      }
      emit({ event: 'failed', folder, name: p.name, message: errorLine(e) });
    }
  }
}

/** Put the files into one `folder` (no folders to make). */
export const uploadFiles = (
  backend: Parameters<typeof uploadItems>[0],
  prepare: Prepare,
  folder: string,
  files: { name: string; data: Uint8Array }[],
  emit: (e: UploadEvent) => void,
  stop?: () => boolean,
): Promise<void> =>
  uploadItems(
    backend,
    prepare,
    [],
    files.map((f) => ({ folder, name: f.name, size: f.data.length, read: async () => f.data })),
    emit,
    stop,
  );
