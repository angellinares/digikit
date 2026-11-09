// The pure logic of the Transfer page (no DOM, no Tauri): the two lists, the install request, the progress bar and the texts.
// Host tests: apps/web/tests/transfer.test.ts.
//
// The page only puts files on the card (upload and verify) and restarts the device. Which modules the device loads is its own
// selection, made in SETTINGS > DIGIKIT > MODULES on the device and applied there.

import { formatBytes } from '../modules/backend.ts';
import { fitsSet } from '../modules/fit.ts';
import type { Group, TransferEvent, TransferRow, TransferView } from '../modules/dktransfer.ts';
export type { Group, TransferEvent, TransferRow, TransferView };
export const GROUPS: Group[] = ['Machines', 'Effects', 'Tools'];

// ---- the mode of the page ---------------------------------------------------------------------------------------------------------

export type Mode = 'modules' | 'samples';
export const MODES: { id: Mode; label: string }[] = [
  { id: 'modules', label: 'Modules' },
  { id: 'samples', label: 'Samples' },
];
/** The saved mode, or Modules when nothing (or something else) was saved. */
export const parseMode = (v: unknown): Mode => MODES.find((m) => m.id === v)?.id ?? 'modules';

/** The free space of a store: in GiB from 1 GiB up, else bytes, KiB or MiB. */
export function freeText(bytes: number): string {
  return bytes >= 2 ** 30 ? `${(bytes / 2 ** 30).toFixed(2)} GiB` : formatBytes(bytes);
}

/** The free space of the device, for the On device column in Samples mode (the target, device and runtime are in the connection badge); '' when unknown. */
export function freeLine(free: number | undefined): string {
  return free === undefined ? '' : `${freeText(free)} free`;
}

/** The storage line of Modules mode, under the device line: the module files on the card (the rows of the list) and the free space of the store. Storage only: no memory. */
export function storageLine(view: TransferView): string {
  const rows = onDevice(view);
  const bytes = rows.reduce((n, r) => n + r.file_size, 0);
  return `Modules: ${rows.length} file${rows.length === 1 ? '' : 's'}, ${formatBytes(bytes)} · ${freeText(view.free_bytes)} free`;
}

/** The key of a row in the tick set. */
export const rowKey = (r: TransferRow): string => r.id ?? r.path;

/** The "On device" list: every file on the card (library modules that are on it, then files the library does not have), by path. */
export function onDevice(view: TransferView): TransferRow[] {
  const mine = view.modules.filter((r) => r.on_card !== 'no');
  return [...mine, ...view.card_only].sort((a, b) => a.path.localeCompare(b.path));
}

/** The "Available" list: the library modules in the three groups (test modules behind the toggle). */
export function available(view: TransferView, showTests: boolean): { group: Group; rows: TransferRow[] }[] {
  return GROUPS.map((group) => ({
    group,
    rows: view.modules.filter((r) => r.group === group && (showTests || !r.test)),
  }));
}

/** The module ids to install: the ticked library modules, in list order. (Ticks mean "install these"; nothing else.) */
export function installModules(view: TransferView, ticks: Set<string>): string[] {
  return view.modules.filter((r) => r.id && ticks.has(rowKey(r))).map((r) => r.id as string);
}

/** A readable line for a progress event of `dktransfer` (`install`, `restart`). */
export function eventText(e: TransferEvent): string {
  const s = (v: unknown) => (Array.isArray(v) ? v.join(', ') : String(v));
  switch (e.event) {
    case 'start':
      return `Installing ${s(e.modules)} on ${e.device}`;
    case 'build':
      return `Building ${e.id}`;
    case 'upload':
      return `Uploading ${e.path} (${e.bytes} bytes)`;
    case 'skip':
      return `${e.id}: already on the card, not uploaded`;
    case 'progress':
      return `Uploading ${e.id}`;
    case 'delete':
      return `Deleting ${e.path}`;
    case 'verify':
      return e.ok ? `Verified ${e.path} on the card` : `Verify failed: ${e.path} differs on the card`;
    case 'restart':
      return 'Restarting the device';
    case 'wait':
      return 'Waiting for the device to boot';
    case 'done':
      if (Array.isArray(e.deleted)) return `Deleted ${e.deleted.length} file${e.deleted.length === 1 ? '' : 's'}`;
      if (e.restarted === false) return DONE_ON_CARD;
      return Array.isArray(e.loaded) && e.loaded.length
        ? `Restarted. Loaded: ${s(e.loaded)}`
        : 'Restarted. No module is loaded.';
    case 'error':
      return `Error: ${e.message}`;
    default:
      return e.event;
  }
}

/** The text after a successful install. */
export const DONE_ON_CARD = 'On the card. Select it in MODULES on the device, then apply and restart.';

/** The fit warning of a row, or ''. */
export function warningText(r: TransferRow): string {
  return r.warnings && r.warnings.length ? r.warnings.join('; ') : '';
}

/** A small label after a row's name: `tone` colours it (`ok` green, `warn` amber, `dim` grey); `title` is its tooltip. */
export interface RowTag {
  text: string;
  title?: string;
  tone: 'ok' | 'warn' | 'dim';
}

/** The tags of a library module in the "On your computer" list: a test module, what the device holds of it, and whether it fits the device
 * (from the needs inferred from its contents, next to the modules loaded now; the tooltip says what is short). A loaded module says nothing of fit. */
export function localTags(r: TransferRow): RowTag[] {
  const out: RowTag[] = [];
  if (r.test) out.push({ text: 'test', tone: 'dim' });
  if (r.on_card === 'yes') out.push({ text: 'on device', tone: 'ok' });
  else if (r.on_card === 'other version')
    out.push({ text: 'differs', title: 'A different build is on the device; Install replaces it', tone: 'warn' });
  const w = warningText(r);
  if (w) out.push({ text: "won't fit", title: w, tone: 'warn' });
  else if (r.fits === true && !r.loaded)
    out.push({ text: 'fits', title: 'Fits the device next to the modules loaded now', tone: 'dim' });
  return out;
}

/**
 * The line for the ticked modules: do they fit the device together? The next boot loads the selection into an empty runtime, so the ticked modules
 * are taken in list order out of the device's limits (the sum rule: it does not model fragmentation, so it is conservative) and what is on the
 * device now is not counted. null when nothing is ticked or the device gave no CAPACITY.
 */
export function tickedFit(view: TransferView, ticks: Set<string>): { text: string; tone: 'ok' | 'warn' } | null {
  const rows = view.modules.filter((r) => r.id && ticks.has(rowKey(r)) && r.needs);
  if (!rows.length || !view.capacity) return null;
  const short = fitsSet(
    rows.map((r) => r.needs!),
    view.capacity,
    true,
  );
  if (!short)
    return {
      text:
        rows.length === 1
          ? 'The ticked module fits the device.'
          : `The ${rows.length} ticked modules fit the device together.`,
      tone: 'ok',
    };
  const row = rows[short.index] as (typeof rows)[number];
  const name = row.label ?? row.name;
  return {
    text:
      short.index === 0
        ? `${name} does not fit the device: ${short.describe}`
        : `${name} does not fit with the ticked modules before it: ${short.describe}`,
    tone: 'warn',
  };
}

/** The tags of a file of the "On device" list: a loaded module says so first; a file the library does not have, and one that differs from
 * the library's build. A file that matches the library has no tag (that is the usual state), and one that is only going there has none yet. */
export function deviceTags(r: TransferRow): RowTag[] {
  const out: RowTag[] = [];
  if (r.loaded) out.push({ text: 'loaded', title: 'Loaded by the device until the next restart', tone: 'ok' });
  if (!r.id)
    out.push({
      text: 'not in library',
      title: 'Only on the device: no built module of your library matches',
      tone: 'dim',
    });
  else if (r.on_card === 'other version')
    out.push({ text: 'differs', title: 'Differs from the build in your library', tone: 'warn' });
  return out;
}

/** Per store path: what VERIFY said after the last install. */
export type Verified = Record<string, 'ok' | 'failed'>;

// ---- the progress of an install or a restart -----------------------------------------------------------------------------------

export type StepState = 'wait' | 'build' | 'upload' | 'verify' | 'ok' | 'failed';
export interface ModuleStep {
  id: string;
  state: StepState;
  /** The store path and the file size, known from the `upload` event (the size of the list's row before). */
  path?: string;
  bytes: number;
  /** Bytes the device acknowledged. */
  sent: number;
}
export interface Progress {
  /** One entry per module of an install, in order (none for a restart). */
  modules: ModuleStep[];
  /** A restart has one step; an install has none. */
  restart: StepState | 'none';
  message: string;
}

/** `sizes`: the file size of each module from the list, so that the bar has its weights before the uploads start. */
export function startProgress(ids: string[], restart: boolean, sizes: Record<string, number> = {}): Progress {
  return {
    modules: ids.map((id) => ({ id, state: 'wait', bytes: sizes[id] ?? 0, sent: 0 })),
    restart: restart ? 'wait' : 'none',
    message: '',
  };
}

const percent = (sent: number, bytes: number) => (bytes ? Math.min(100, Math.round((100 * sent) / bytes)) : 0);

/** The progress after one more event of `dktransfer` (a new object; `error` marks the active step failed). `result` and `progress`
 * lines are not steps: the text stays that of the last step (a `progress` line only updates the percent). */
export function applyEvent(p: Progress, e: TransferEvent): Progress {
  const next: Progress = { ...p, modules: p.modules.map((m) => ({ ...m })) };
  if (e.event !== 'result') next.message = eventText(e);
  const mod = (id: unknown) => next.modules.find((m) => m.id === id);
  switch (e.event) {
    case 'build': {
      const m = mod(e.id);
      if (m) m.state = 'build';
      break;
    }
    case 'upload': {
      const m = mod(e.id);
      if (m) Object.assign(m, { state: 'upload', path: String(e.path), bytes: Number(e.bytes), sent: 0 });
      break;
    }
    case 'progress': {
      const m = mod(e.id);
      if (m) {
        m.bytes = Number(e.bytes);
        m.sent = Number(e.sent);
        next.message = `Uploading ${m.path ?? m.id}: ${percent(m.sent, m.bytes)}%`;
      }
      break;
    }
    case 'skip': {
      const m = mod(e.id); // the upload is not needed: straight to the verify
      if (m) m.sent = m.bytes;
      break;
    }
    case 'verify': {
      const m = mod(e.id);
      if (m) Object.assign(m, { state: e.ok ? 'ok' : 'failed', sent: m.bytes });
      break;
    }
    case 'delete': {
      // a delete job has one step per path (the module list holds the paths): the one being deleted is active, the earlier ones are done
      for (const m of next.modules) if (m.state === 'verify') m.state = 'ok';
      const m = mod(e.path);
      if (m) m.state = 'verify';
      break;
    }
    case 'restart':
      next.restart = 'build';
      break;
    case 'done':
      if (next.restart !== 'none') next.restart = 'ok';
      if (Array.isArray(e.deleted)) for (const m of next.modules) m.state = 'ok';
      break;
    case 'error': {
      const active = next.modules.find((m) => m.state === 'build' || m.state === 'upload' || m.state === 'verify');
      if (active) active.state = 'failed';
      else if (next.restart === 'build') next.restart = 'failed';
      break;
    }
  }
  return next;
}

/** The build and verify steps of a module count as this many bytes each; the uploads count by their bytes. */
export const FIXED_SHARE = 2048;

/** The overall fraction, 0 to 1: per module a fixed share for the build, its bytes sent, and a fixed share for the verify. */
export function progressFraction(p: Progress): number {
  if (p.restart !== 'none' && p.modules.length === 0) return p.restart === 'ok' ? 1 : 0;
  let total = 0;
  let done = 0;
  for (const m of p.modules) {
    total += 2 * FIXED_SHARE + m.bytes;
    if (m.state === 'upload' || m.state === 'verify' || m.state === 'ok') done += FIXED_SHARE + m.sent; // the build is over
    if (m.state === 'ok') done += FIXED_SHARE;
  }
  return total ? Math.min(1, done / total) : 0;
}

/** Did a step fail? (the bar turns red) */
export function progressFailed(p: Progress): boolean {
  return p.restart === 'failed' || p.modules.some((m) => m.state === 'failed');
}

/** What the bar along the bottom shows, the same for an install, a delete, a restart and a samples upload. */
export interface BarInfo {
  /** The overall fraction, 0 to 1 (the bar). */
  fraction: number;
  failed: boolean;
  message: string;
  /** Bytes of files sent so far and in all (the speed and the time left come from these; 0 in all: no bytes in this job). */
  sent: number;
  total: number;
}

/** The bar of an install, delete or restart. */
export function modulesBar(p: Progress): BarInfo {
  let sent = 0;
  let total = 0;
  for (const m of p.modules) {
    total += m.bytes;
    sent += m.state === 'ok' ? m.bytes : m.state === 'upload' || m.state === 'verify' ? m.sent : 0;
  }
  return { fraction: progressFraction(p), failed: progressFailed(p), message: p.message || 'Starting…', sent, total };
}

/** What one file's row shows while an install runs: its upload bar, "verifying", or the result. */
export type RowStatus =
  { kind: 'uploading'; sent: number; bytes: number } | { kind: 'verifying' } | { kind: 'ok' } | { kind: 'failed' };

/** The status of the file at `path`: from the install's step when it has one, else from the last VERIFY. */
export function rowStatus(p: Progress | undefined, verified: Verified, path: string): RowStatus | undefined {
  const m = p?.modules.find((x) => x.path === path);
  if (m) {
    if (m.state === 'upload')
      return m.sent < m.bytes ? { kind: 'uploading', sent: m.sent, bytes: m.bytes } : { kind: 'verifying' };
    if (m.state === 'ok') return { kind: 'ok' };
    if (m.state === 'failed') return { kind: 'failed' };
  }
  const v = verified[path];
  return v ? { kind: v } : undefined;
}

/** The "On device" rows plus, for each module of the running install whose file is not on the card yet, a pending row (the file is going
 * there: the upload bar sits in it). */
export function onDeviceWithPending(view: TransferView, p: Progress | undefined): TransferRow[] {
  const rows = onDevice(view);
  const have = new Set(rows.map((r) => r.path));
  for (const m of p?.modules ?? []) {
    if (!m.path || have.has(m.path)) continue;
    const repo = view.modules.find((r) => r.id === m.id);
    rows.push({
      ...(repo ?? { group: 'Tools', test: false, name: m.id, version: null }),
      id: m.id,
      path: m.path,
      file_size: m.bytes,
      on_card: 'no',
      loaded: false,
    } as TransferRow);
    have.add(m.path);
  }
  return rows.sort((a, b) => a.path.localeCompare(b.path));
}

/** The verified map after more events: a `verify` event sets its path. */
export function applyVerified(v: Verified, e: TransferEvent): Verified {
  if (e.event !== 'verify' || typeof e.path !== 'string') return v;
  return { ...v, [e.path]: e.ok ? 'ok' : 'failed' };
}

// ---- delete -----------------------------------------------------------------------------------------------------------------------

/** The store paths ticked in the On device pane, in list order (ticks of rows that are gone are dropped). */
export function deletePaths(view: TransferView, ticked: Set<string>): string[] {
  return onDevice(view)
    .filter((r) => ticked.has(r.path))
    .map((r) => r.path);
}

/** A button label with the count in parentheses: "Upload (3)"; no count when nothing is ticked. */
export const countLabel = (label: string, n: number): string => (n > 0 ? `${label} (${n})` : label);

/** The label of the Delete button: the count, and "Confirm" on the first click. */
export const deleteLabel = (n: number, confirming: boolean): string =>
  n > 0 && confirming ? countLabel('Confirm delete', n) : countLabel('Delete', n);

/** The text after a delete job: a module that was loaded stays loaded until the restart (DELETE does not touch the device's selection). */
export function deleteDoneText(view: TransferView, paths: string[]): string {
  const rows = onDevice(view).filter((r) => paths.includes(r.path));
  const loaded = rows.filter((r) => r.loaded).map((r) => r.label ?? r.name);
  const base = `Deleted ${paths.length} file${paths.length === 1 ? '' : 's'}.`;
  return loaded.length ? `${base} ${loaded.join(', ')}: deleted; unloads at restart.` : base;
}

/** A job that writes to the device (the target cannot change under it); reading and waiting can be left by choosing another target. */
export function writing(busy: string): boolean {
  return busy !== '' && busy !== 'Reading';
}
