// The Samples page logic of the Transfer page (no DOM, no Tauri): breadcrumbs, the rows of the local folder and of the device's folder with
// the state of each upload, the bar of a run, the plan of an upload with folders, and the delete texts. The upload engine (upload state, events,
// the run over a samples backend) is in sample-upload.ts.
// Written without TS-only runtime syntax so node can import it (packages/workbench-core/tests/samples-core.test.ts).
import type { Prepared } from '@digi/dk-session/dksample';
import type { SampleEntry } from '@digi/dk-session/samples';
import { viaSuffix, type LinkState } from './link-core.ts';
import { fmtRate } from './rate-core.ts';
import { cancelledCounts, joinPath, SAVING_MESSAGE, type Upload, type Uploads } from './sample-upload.ts';
import type { BarInfo, RowStatus, RowTag } from './transfer-core.ts';

// ---- folders ----------------------------------------------------------------------------------------------------------------------

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
  sent?: number;
  bytes?: number;
  /** Empty while sending; the device path during a check, for the delayed hint. */
  checking?: string;
};
export function uploadStatus(u: Upload | undefined): SampleStatus | undefined {
  if (!u) return undefined;
  const message = u.message;
  switch (u.state) {
    case 'waiting':
      return { kind: 'waiting', message };
    case 'uploading':
      return { kind: 'uploading', sent: u.sent, bytes: u.bytes, message, checking: '' };
    case 'verifying':
      // saving (pending on the device): a label, not the full bar of a check
      if (message === SAVING_MESSAGE) return { kind: 'verifying', message, checking: '' };
      return { kind: 'verifying', sent: u.bytes, bytes: u.bytes, message, checking: joinPath(u.folder, u.name) };
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

/** The bar of an upload run: the uploads that this run touched (`base` is what `uploads` held when the run started; an entry that is the very
 * same object was not touched). `running`: the run is still going, so the message names the file. `average`: bytes per second of the whole
 * run, for the end text. A cancelled run is not red, and the bar shows the bytes that went, never more. No entry: no bar (undefined). */
export function samplesBar(
  uploads: Uploads,
  base: Uploads,
  running: boolean,
  average?: number,
  link?: LinkState,
): BarInfo | undefined {
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
  // Keep one job label across sending, checking and preparation of the next file.
  const current = mine.find(([, u]) => u.state === 'waiting' || u.state === 'uploading' || u.state === 'verifying');
  const plural = (k: number) => `${k} file${k === 1 ? '' : 's'}`;
  let message: string;
  if (running) {
    const file = current?.[1] ?? mine[n - 1]![1];
    message = `Transferring ${Math.min(n, ok + same + failed + cancelled + 1)} of ${n} · ${file.source}${viaSuffix(link)}`;
  } else if (failed)
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
    checking: running && current?.[1].state === 'verifying' ? current[0] : '',
  };
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
    const checking = open.find((u) => u.state === 'verifying');
    return inside.every((u) => u.state === 'waiting')
      ? { kind: 'waiting', message: '' }
      : {
          kind: 'uploading',
          sent,
          bytes,
          message: '',
          checking: checking ? joinPath(checking.folder, checking.name) : '',
        };
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
