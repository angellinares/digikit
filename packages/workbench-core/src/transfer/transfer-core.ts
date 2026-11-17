// The pure logic of the Transfer page (no DOM, no Tauri): the two lists, the install request, the progress bar and the texts.
// Host tests: apps/web/tests/transfer.wasm.test.ts.
//
// The page only puts files on the card (upload and verify) and restarts the device. Which modules the device loads is its own
// selection, made in SETTINGS > DIGIKIT > MODULES on the device and applied there.

import { formatBytes } from '../modules/backend.ts';
import { fit, fitsSet, type Needs } from '../modules/fit.ts';
import type { Group, TransferEvent, TransferRow, TransferView } from '../modules/dktransfer.ts';
import { PROVIDER_MAX } from '../modules/services.ts';
import type { ServiceRef, ServiceRefusal } from '../modules/services.ts';
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

/** The services of the ticked modules that can install: each service once, in the order of the rows (a refused module adds none). */
function tickedServices(view: TransferView, ticks: Set<string>): ServiceRef[] {
  const seen = new Set<string>();
  const out: ServiceRef[] = [];
  for (const r of view.modules) {
    if (!r.id || !ticks.has(rowKey(r)) || r.service_refusal) continue;
    for (const sv of r.services ?? [])
      if (!seen.has(sv.stem)) {
        seen.add(sv.stem);
        out.push(sv);
      }
  }
  return out;
}

/** The capacity needs of the ticked modules' services, each service once (a service without needs adds nothing). */
function serviceNeeds(view: TransferView, ticks: Set<string>): Needs[] {
  return tickedServices(view, ticks).flatMap((sv) => (sv.needs ? [sv.needs] : []));
}

/** The services to upload for the ticked modules, each once, dependencies first; a service that is on the device with the same hash is left out. */
export function installServices(view: TransferView, ticks: Set<string>): { id: string; path: string; bytes: number }[] {
  return tickedServices(view, ticks)
    .filter((sv) => sv.state !== 'on device')
    .map((sv) => ({ id: sv.stem, path: `services/${sv.stem}.dks`, bytes: sv.size }));
}

const plural = (n: number, one: string, many: string) => `${n} ${n === 1 ? one : many}`;

/** The count text of a pane header: nothing ticked gives `12 items` (no lead), some ticked gives lead `3` and rest ` of 12 selected`. */
export function tickCountText(ticked: number, total: number): { lead: string; rest: string } {
  if (ticked > 0) return { lead: String(ticked), rest: ` of ${total} selected` };
  return { lead: '', rest: total > 0 ? plural(total, 'item', 'items') : '' };
}

/** The text of a refused module: nothing was uploaded for it. */
export function serviceRefusalText(r: ServiceRefusal): string {
  switch (r.kind) {
    case 'version':
      return `${r.module} not installed. It needs ${r.service} service ${r.need}, the runtime has ${r.have}.`;
    case 'too-many':
      return `${r.module} not installed. The selection needs more than ${PROVIDER_MAX} services.`;
    case 'fingerprint': {
      const versions = r.runtime && r.builtFor ? ` (runtime ${r.runtime}, service built for ${r.builtFor})` : '';
      return (
        `${r.module} not installed. It needs the ${r.service} service, and this workbench has no ${r.service} service for the runtime on your device${versions}. ` +
        `Nothing was uploaded for ${r.module}. Update the workbench, or flash a matching runtime from the Firmware page.`
      );
    }
    case 'no-stock':
      return (
        `${r.module} not installed. It needs the ${r.service} service, and the workbench prepares services from your own firmware file. ` +
        `Choose that file once on the Firmware page. Nothing was uploaded for ${r.module}.`
      );
    default:
      return `${r.module} not installed. It needs the ${r.service} service, which this workbench does not have.`;
  }
}

/** The refusal of a delete of a service that a module on the card still needs. */
export const neededServiceText = (service: string, by: string[]): string =>
  `${service} service is needed by ${by.join(', ')}. Delete that module first (its services go with it).`;

/** The lines after a delete job, from its `delete` and `keep` events, then the restart hint. */
export function deleteResultLines(events: TransferEvent[]): string[] {
  const lines: string[] = [];
  for (const e of events) {
    if (e.event === 'delete')
      lines.push(`Deleted ${e.path}${e.why === 'unused-service' ? ' (no module on the card needs it)' : ''}`);
    else if (e.event === 'keep') lines.push(`Kept ${e.path} (${(e.by ?? []).join(', ')} needs it)`);
  }
  if (lines.length) lines.push('Restart the device to apply.');
  return lines;
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
      return `${e.kind === 'service' ? (e.path ?? e.id) : e.id}: already on the card, not uploaded`;
    case 'progress':
      return `Uploading ${e.kind === 'service' ? (e.path ?? e.id) : e.id}`;
    case 'refuse':
      return serviceRefusalText(e.refusal!);
    case 'keep':
      return `Kept ${e.path} (${(e.by ?? []).join(', ')} needs it)`;
    case 'delete':
      return e.why === 'unused-service' ? `Deleted ${e.path} (no module on the card needs it)` : `Deleting ${e.path}`;
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

/** The tags of a library module in the "On your computer" list: a test module, what the device holds of it, and a module that does not fit the
 * device (from the needs inferred from its contents, next to the modules loaded now; the tooltip says what is short). Fitting is the usual
 * state and says nothing; the line above the list says how the ticked ones fit together. */
export function localTags(r: TransferRow): RowTag[] {
  const out: RowTag[] = [];
  if (r.test) out.push({ text: 'test', tone: 'dim' });
  if (r.on_card === 'yes') out.push({ text: 'on device', tone: 'ok' });
  else if (r.on_card === 'other version')
    out.push({ text: 'differs', title: 'A different build is on the device; Install replaces it', tone: 'warn' });
  const w = warningText(r);
  if (w) out.push({ text: "won't fit", title: w, tone: 'warn' });
  const sr = r.service_refusal;
  if (sr)
    out.push({
      text: 'service mismatch',
      title:
        sr.builtFor && sr.runtime
          ? `${sr.service} service built for ${sr.builtFor}, device runtime is ${sr.runtime}`
          : serviceRefusalText(sr),
      tone: 'warn',
    });
  return out;
}

/** The tag of a service sub row under a module: where the service stands on the device. */
export function serviceTags(sv: ServiceRef): RowTag[] {
  return [
    {
      text: sv.state,
      tone: sv.state === 'on device' ? 'ok' : 'warn',
      title: sv.state === 'differs' ? 'A different build is on the device; Install replaces it' : undefined,
    },
  ];
}

/**
 * The line for the ticked modules: do they fit the device together? The next boot loads the selection into an empty runtime, so the ticked modules
 * are taken in list order out of the device's limits (the sum rule: it does not model fragmentation, so it is conservative) and what is on the
 * device now is not counted. null when nothing is ticked or the device gave no CAPACITY.
 */
export function tickedFit(view: TransferView, ticks: Set<string>): { text: string; tone: 'ok' | 'warn' } | null {
  const rows = view.modules.filter((r) => r.id && ticks.has(rowKey(r)) && r.needs && !r.service_refusal);
  if (!rows.length || !view.capacity) return null;
  const nServices = tickedServices(view, ticks).length;
  if (nServices > PROVIDER_MAX)
    return { text: `The selection needs more than ${PROVIDER_MAX} services.`, tone: 'warn' };
  const short = fitsSet([...rows.map((r) => r.needs!), ...serviceNeeds(view, ticks)], view.capacity, true);
  if (!short) {
    const withServices = nServices
      ? rows.length === 1
        ? ` and its ${plural(nServices, 'service', 'services')}`
        : ` and their ${plural(nServices, 'service', 'services')}`
      : '';
    return {
      text:
        rows.length === 1
          ? `The ticked module${withServices} ${nServices ? 'fit' : 'fits'} the device.`
          : `The ${rows.length} ticked modules${withServices} fit the device together.`,
      tone: 'ok',
    };
  }
  // an index past the modules is one of their services (counted after the modules)
  if (short.index >= rows.length)
    return {
      text: `The services of the ticked modules do not fit the device with them: ${short.describe}`,
      tone: 'warn',
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

/** The load of the device after the next boot with the ticked modules (`DSP 52% CPU 1%`, the fit rule's load line), or null when nothing is
 * ticked, the device gave no CAPACITY or the ticked modules do not fit together (tickedFit says which). */
export function tickedLoad(view: TransferView, ticks: Set<string>): string | null {
  const rows = view.modules.filter((r) => r.id && ticks.has(rowKey(r)) && r.needs && !r.service_refusal);
  if (!rows.length || !view.capacity || tickedServices(view, ticks).length > PROVIDER_MAX) return null;
  const answer = fit({
    caps: view.capacity,
    needs: [...rows.map((r) => r.needs!), ...serviceNeeds(view, ticks)],
    empty: true,
  });
  return answer.set || !answer.line ? null : answer.line;
}

/** The tags of a file of the "On device" list: a loaded module says so first, a selected one that failed to load says why (`not_loaded`); a file the library does not have, and one that differs from
 * the library's build. A file that matches the library has no tag (that is the usual state), and one that is only going there has none yet. */
export function deviceTags(r: TransferRow): RowTag[] {
  const out: RowTag[] = [];
  if (r.loaded) out.push({ text: 'loaded', title: 'Loaded by the device until the next restart', tone: 'ok' });
  if (r.not_loaded)
    out.push({
      text: r.load_state === 'REFUSED' ? 'refused' : r.load_state === 'SKIPPED' ? 'skipped' : 'not loaded',
      title: r.not_loaded,
      tone: 'warn',
    });
  if (r.needed_by?.length)
    out.push({
      text: `needed by ${r.needed_by.join(', ')}`,
      title: 'A module on the card needs this service; delete that module first',
      tone: 'dim',
    });
  else if (!r.id)
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
  /** 'service' for a service file (those steps come first); absent: a module. */
  kind?: 'service' | 'module';
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
  /** Modules the install refused (a service check failed): not uploaded, not steps. */
  refused?: ServiceRefusal[];
}

/** `sizes`: the file size of each module from the list, so that the bar has its weights before the uploads start. `services`: the service
 * files to upload (installServices); their steps come first, as the install uploads services before modules. */
export function startProgress(
  ids: string[],
  restart: boolean,
  sizes: Record<string, number> = {},
  services: { id: string; path: string; bytes: number }[] = [],
): Progress {
  return {
    modules: [
      ...services.map((sv): ModuleStep => ({
        id: sv.id,
        kind: 'service',
        state: 'wait',
        path: sv.path,
        bytes: sv.bytes,
        sent: 0,
      })),
      ...ids.map((id): ModuleStep => ({ id, state: 'wait', bytes: sizes[id] ?? 0, sent: 0 })),
    ],
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
  const service = 'kind' in e && e.kind === 'service';
  const mod = (id: unknown) =>
    next.modules.find(
      (m) =>
        (m.kind === 'service') === service && (m.id === id || (service && m.path === (e as { path?: unknown }).path)),
    );
  switch (e.event) {
    case 'refuse': {
      // the module is not uploaded: it leaves the steps, the others go on
      next.modules = next.modules.filter((m) => m.kind === 'service' || m.id !== e.refusal!.module);
      next.refused = [...(next.refused ?? []), e.refusal!];
      break;
    }
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
  return p.restart === 'failed' || p.modules.some((m) => m.state === 'failed') || allRefused(p);
}

/** Was every ticked module refused (so that nothing was uploaded)? */
export const allRefused = (p: Progress): boolean => !!p.refused?.length && !p.modules.some((m) => m.kind !== 'service');

/** The lines after an install that had refusals: each refused module named with its reason, then `x was installed.` for the others
 * (`ids`: the ticked module ids). Empty when nothing was refused. */
export function installResultLines(p: Progress, ids: string[]): string[] {
  const refused = p.refused ?? [];
  if (!refused.length) return [];
  const names = new Set(refused.map((r) => r.module));
  const leaf = (id: string) => id.split('/').pop() as string;
  return [
    ...refused.map(serviceRefusalText),
    ...ids.filter((id) => !names.has(id) && !names.has(leaf(id))).map((id) => `${leaf(id)} was installed.`),
  ];
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
  /** Samples retain their byte bars through verification. Empty between checks; a path identifies the current check. */
  checking?: string;
}

/** The bar of an install, delete or restart. */
export function modulesBar(p: Progress): BarInfo {
  let sent = 0;
  let total = 0;
  for (const m of p.modules) {
    total += m.bytes;
    sent += m.state === 'ok' ? m.bytes : m.state === 'upload' || m.state === 'verify' ? m.sent : 0;
  }
  const message = allRefused(p) ? 'Install failed: no files uploaded' : p.message || 'Starting…';
  return { fraction: progressFraction(p), failed: progressFailed(p), message, sent, total };
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
    const repo = m.kind === 'service' ? undefined : view.modules.find((r) => r.id === m.id);
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
