// The sample upload engine: the upload state and its events, and the run of a list of files over a samples backend
// (packages/dk-session/src/samples.ts), with the summaries of a run. Pure logic, no DOM or Tauri. It imports no Transfer UI code
// (transfer-core, link-core, rate-core), so runtime tests can use it. The Samples page logic is in samples-core.ts.
// Written without TS-only runtime syntax so node can import it (packages/workbench-core/tests/sample-upload.test.ts).
import { errorLine, isCancelled } from '../modules/backend.ts';
import type { Prepared } from '@digi/dk-session/dksample';
import type { SampleEntry, SamplesBackend } from '@digi/dk-session/samples';

// ---- folders ----------------------------------------------------------------------------------------------------------------------

export const joinPath = (folder: string, name: string): string => (folder ? `${folder}/${name}` : name);
export const parentOf = (folder: string): string => folder.split('/').slice(0, -1).join('/');

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
  /** The device verified the file and holds it pending: it is named in the store with the next commit (the batch). */
  | { event: 'saving'; folder: string; name: string }
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
    case 'saving':
      return { ...u, [key]: { ...base, state: 'verifying', sent: base.bytes, message: SAVING_MESSAGE } };
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

/** The label of a file that the device verified and holds pending (the chip says it instead of "verifying…"). */
export const SAVING_MESSAGE = 'Saving…';
/** The tooltip of a file that was saved with a batch commit. */
export const SAVED_MESSAGE =
  'The device checked this file when it was received (read back from the card and hashed) and saved it with the files of its batch.';
/** The tooltip of a file that the device verified but whose saving was not confirmed: the link or the commit failed. The device may still commit it by itself 1 s after its last file call, so the next run's `sameOnDevice` skips it when it did. */
export const NOT_CONFIRMED = 'not confirmed, run again';

/** The tooltip of a file that was not sent: what was compared, and how to get a fresh read-back. */
export const SAME_MESSAGE =
  'The device has this file with the same size and content hash: not sent, not read back. To read it back from the card, delete it and upload it again.';

/** "87 of 357 uploaded, 270 not sent": what a stopped run did; files that were on the device already are named, not counted as not sent. */
export const cancelledCounts = (ok: number, n: number, same = 0): string =>
  `${ok} of ${n} uploaded, ${same ? `${same} already on the device, ` : ''}${n - ok - same} not sent`;

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
 *
 * A backend with `commit` gets files with `defer` (the batch commit): the device verifies each file at its END but names it in the store only with the next
 * commit. A file whose reply says files are pending stays `verifying`, labelled "Saving…" (no SysEx verify: the device checked it). When a reply shows nothing
 * pending, or the commit sent after the last file succeeds, the files waiting become `ok`. When the commit fails or the bulk link fell back with files waiting,
 * they become `failed` ("not confirmed, run again": the device may still commit them itself after 1 s); the next run's `sameOnDevice` then sends only the
 * files the device does not hold. After a link loss the run waits `settleMs` and lists the folders again before it goes on, so files the device committed
 * meanwhile are seen (a file that the fresh listing shows with the same bytes is `ok`, the others `failed`). A stop still commits the waiting files (verified
 * data is never dropped) and shows them `ok`; the commit gets an abort signal that a stop raised during it fires (a stop before it gives it
 * `commitCancelMs`), so a hung COMMIT is not waited out: those files are "not confirmed". A commit that answers files still pending (a file it replaces is being
 * read) is asked again up to 3 times.
 */
export async function uploadItems(
  backend: Pick<SamplesBackend, 'put' | 'verify'> &
    Partial<Pick<SamplesBackend, 'list' | 'mkdir' | 'sameOnDevice' | 'commit' | 'bulkState'>>,
  prepare: Prepare,
  mkdirs: readonly string[],
  items: readonly UploadItem[],
  emit: (e: UploadEvent) => void,
  stop: () => boolean = () => false,
  { settleMs = 1500, commitCancelMs = 5000 }: { settleMs?: number; commitCancelMs?: number } = {},
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
  // files the device verified and holds pending: named in the store by the next reply that shows 0 pending, or by the commit after the last file
  let waiting: { folder: string; name: string; p: Prepared }[] = [];
  const saved = () => {
    for (const w of waiting)
      emit({ event: 'verified', folder: w.folder, name: w.name, ok: true, message: SAVED_MESSAGE });
    waiting = [];
  };
  const lostWaiting = (why: string) => {
    for (const w of waiting)
      emit({ event: 'failed', folder: w.folder, name: w.name, message: `${NOT_CONFIRMED}: ${why}` });
    waiting = [];
  };
  const pause = (ms: number) => (ms > 0 ? new Promise<void>((r) => setTimeout(r, ms)) : Promise.resolve());
  // the bulk link was lost with files waiting: the device may still commit them (its 1 s idle commit). Wait, list the folders again, and say what the listing shows.
  const afterLoss = async () => {
    const gone = waiting;
    waiting = [];
    await pause(settleMs);
    for (const folder of new Set(gone.map((w) => w.folder))) listings.delete(folder);
    for (const w of gone) {
      let have: SampleEntry[] | null = null;
      try {
        have = await listingOf(w.folder);
      } catch {
        cancelled = true;
      }
      if (have && backend.sameOnDevice?.(w.p, have))
        emit({ event: 'verified', folder: w.folder, name: w.name, ok: true, message: SAVED_MESSAGE });
      else
        emit({
          event: 'failed',
          folder: w.folder,
          name: w.name,
          message: `${NOT_CONFIRMED}: the USB link was lost`,
        });
    }
  };
  // The next file is read and prepared while this one is sent (the host spends 10 to 21 s of a 300-file run on it). The promise never rejects: a failure is a
  // value that the next iteration reports on that file, so nothing is left unhandled when a stop, a cancel or a failed send discards it.
  type Readied = { ok: true; p: Prepared } | { ok: false; error: unknown };
  const readyItem = (it: UploadItem): Promise<Readied> =>
    (async (): Promise<Readied> => {
      try {
        return { ok: true, p: prepare(it.name, await it.read()) };
      } catch (error) {
        return { ok: false, error };
      }
    })();
  let ahead: { index: number; ready: Promise<Readied> } | undefined;
  for (const [index, it] of items.entries()) {
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
    const got = await (ahead?.index === index ? ahead.ready : readyItem(it));
    ahead = undefined;
    if (!got.ok) {
      emit({ event: 'failed', folder, name: it.name, source: it.name, message: errorLine(got.error) });
      continue;
    }
    const p = got.p;
    emit({
      event: 'prepared',
      folder,
      name: p.name,
      source: it.name,
      kind: p.kind,
      bytes: p.stored.length,
      queued: it.name,
    });
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
    const nextItem = items[index + 1];
    if (nextItem && !stop()) ahead = { index: index + 1, ready: readyItem(nextItem) };
    const abort = new AbortController();
    const watch = setInterval(() => {
      if (stop()) abort.abort();
    }, 50);
    let result: Awaited<ReturnType<typeof backend.put>>;
    try {
      result = await backend.put(
        folder,
        p,
        (sent, bytes) => emit({ event: 'progress', folder, name: p.name, sent, bytes }),
        { signal: abort.signal, exists, defer: backend.commit !== undefined },
      );
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
    // the bulk link failed during this file: what the device held pending is not known to be saved
    if (waiting.length && backend.bulkState === 'fallback') await afterLoss();
    if (result && (result.pending > 0 || waiting.length)) {
      // the batch: the device checked the file at END
      waiting.push({ folder, name: p.name, p });
      if (result.pending > 0) emit({ event: 'saving', folder, name: p.name });
      else saved();
      continue;
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
  // the commit after the last file (also after a stop or a failed file: the waiting files are verified data)
  if (waiting.length) {
    if (backend.bulkState === 'fallback' || !backend.commit) await afterLoss();
    else {
      // a stop that comes during the commit ends the wait; a stop that came before gives the commit a bounded time
      const abort = new AbortController();
      const stoppedAtStart = cancelled || stop();
      const timer = stoppedAtStart ? setTimeout(() => abort.abort(), commitCancelMs) : undefined;
      const watch = setInterval(() => {
        if (!stoppedAtStart && stop()) abort.abort();
      }, 50);
      try {
        let left = await backend.commit(abort.signal);
        for (let again = 0; left > 0 && again < 3; again++) {
          await pause(settleMs);
          left = await backend.commit(abort.signal);
        }
        if (left > 0) lostWaiting('the device still held files back');
        else saved();
      } catch (e) {
        lostWaiting(isCancelled(e) ? 'cancelled while saving' : errorLine(e));
      } finally {
        clearInterval(watch);
        if (timer) clearTimeout(timer);
      }
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
