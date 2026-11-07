// The pure logic of the workbench Log drawer (no DOM, no Tauri): the entries and their cap, the filter, the badge, the copy text, the
// rule that opens the drawer by itself, and the one-line texts of the events the app logs. Host tests: apps/desktop/ui/tests/log.test.ts.
import type { ReloadReport } from '../../../web/src/runtime/reload.ts';
import type { FirmwareRun } from '../../../web/src/firmware/env.ts';
import type { TransferRun } from '../../../web/src/transfer/env.ts';
import type { CardFolder } from './card-core.ts';
import { shortFolder } from './card-core.ts';
import type { RuntimeEvent } from '@digi/dk-session';

/** What the app logs about itself. */
export const APP_SOURCES = ['reload', 'build', 'firmware', 'card', 'transfer', 'reset'] as const;
/** The origins of a runtime log: the emulator's and the real device's. */
export const ORIGINS = ['emulator', 'device'] as const;
export type Origin = (typeof ORIGINS)[number];
/** `emulator` is also the app's own entries about the emulator (health, faults, stalls, pause). */
export const LOG_SOURCES = [...APP_SOURCES, 'emulator', 'device'] as const;
export type LogSource = (typeof LOG_SOURCES)[number];
export type LogLevel = 'info' | 'error';

/** What an entry has when it comes from the runtime log of an origin (and not from the app). */
export interface RuntimeMeta {
  origin: Origin;
  /** The module that wrote the event; '' for the runtime itself. */
  module: string;
  /** The event, for the Save file; entries made from the log (a fault, a gap, a note) have none. */
  event?: RuntimeEvent;
  /** The readable line of the event (as `dklog` writes it), for the Save file. */
  line?: string;
}

export interface LogInput {
  source: LogSource;
  level: LogLevel;
  /** One line. */
  text: string;
  /** Longer output (a compiler log); shown when the entry is expanded. */
  detail?: string;
  /** Set for the entries of a runtime log. */
  rt?: RuntimeMeta;
}
export interface LogEntry extends LogInput {
  id: number;
  /** ms since the epoch. */
  at: number;
}
export interface LogState {
  entries: LogEntry[];
  /** Ids only grow, also after a clear, so a "seen" mark never matches a later entry by mistake. */
  nextId: number;
}

/** The log keeps this many entries of the app; the oldest go first. */
export const LOG_CAP = 500;
/** The same for the runtime log of each origin (the device's ring holds 512 events). */
export const RUNTIME_CAP = 2000;

export const emptyLog = (): LogState => ({ entries: [], nextId: 1 });

/** The entries that share a cap: the app's, and each origin's runtime log. */
const bucket = (e: LogInput): string => (e.rt ? `rt:${e.rt.origin}` : 'app');
const capOf = (b: string): number => (b === 'app' ? LOG_CAP : RUNTIME_CAP);

/** Add entries received at `at` (a batch from one poll). */
export function addEntries(state: LogState, inputs: LogInput[], at: number): LogState {
  if (inputs.length === 0) return state;
  let nextId = state.nextId;
  let entries = [...state.entries, ...inputs.map((input): LogEntry => ({ ...input, id: nextId++, at }))];
  const count = new Map<string, number>();
  for (const e of entries) count.set(bucket(e), (count.get(bucket(e)) ?? 0) + 1);
  const drop = new Map([...count].map(([b, n]) => [b, n - capOf(b)] as const).filter(([, over]) => over > 0));
  if (drop.size)
    entries = entries.filter((e) => {
      const over = drop.get(bucket(e));
      if (!over) return true;
      drop.set(bucket(e), over - 1);
      return false;
    });
  return { entries, nextId };
}

export const addEntry = (state: LogState, input: LogInput, at: number): LogState => addEntries(state, [input], at);

export const clearEntries = (state: LogState): LogState => ({ entries: [], nextId: state.nextId });

/**
 * What the Source menu selects: `all`; an app source (`reload`, `build`, `firmware`, `card`, `transfer`, `reset`) or `emulator` (the app's
 * entries about the emulator: health, faults, stalls); or an origin's runtime log, `emulator:all`, `emulator:runtime`, `emulator:mod:NAME`
 * and the same for `device`.
 */
export interface LogFilter {
  source: string;
  errorsOnly: boolean;
}

export const originKey = (origin: Origin, part: 'all' | 'runtime' | { module: string }): string =>
  typeof part === 'string' ? `${origin}:${part}` : `${origin}:mod:${part.module}`;

export function matchesSource(e: LogEntry, source: string): boolean {
  if (source === 'all') return true;
  const [origin, part, ...name] = source.split(':');
  if (part === undefined) return e.source === source && !e.rt;
  if (e.source !== origin) return false;
  if (part === 'all') return true;
  if (!e.rt) return false;
  return part === 'runtime' ? e.rt.module === '' : e.rt.module === name.join(':');
}

export const filterEntries = (entries: LogEntry[], filter: LogFilter): LogEntry[] =>
  entries.filter((e) => matchesSource(e, filter.source) && (!filter.errorsOnly || e.level === 'error'));

export interface SourceOption {
  value: string;
  label: string;
  disabled?: boolean;
  title?: string;
}
export interface SourceGroup {
  label: string;
  options: SourceOption[];
}

/** What the Source menu needs to know of an origin. */
export interface OriginMenu {
  /** Something can be followed now (the emulator runs, the device is there and has the runtime log). */
  live: boolean;
  /** The origin has entries in the log. */
  logged: boolean;
  /** The modules it has loaded. */
  modules: string[];
}

/** The groups of the Source menu: App, Emulator, and the device on the MIDI port. A group with nothing to follow or show has one disabled option. */
export function sourceMenu(o: { emulator: OriginMenu; device: OriginMenu & { port?: string } }): SourceGroup[] {
  const origin = (name: Origin, m: OriginMenu, none: string): SourceOption[] =>
    m.live || m.logged
      ? [
          { value: originKey(name, 'all'), label: 'all' },
          ...(name === 'emulator'
            ? [{ value: 'emulator', label: 'emulator', title: 'Health, faults and stalls of the emulator' }]
            : []),
          { value: originKey(name, 'runtime'), label: 'runtime' },
          ...m.modules.map((module) => ({ value: originKey(name, { module }), label: module })),
        ]
      : [{ value: '', label: none, disabled: true }];
  return [
    { label: 'App', options: APP_SOURCES.map((value) => ({ value, label: value })) },
    { label: 'Emulator', options: origin('emulator', o.emulator, 'no runtime running') },
    {
      label: o.device.port ? `Device: MIDI ${o.device.port}` : 'Device',
      options: origin('device', o.device, o.device.port ? 'no runtime running' : 'no MIDI port'),
    },
  ];
}

/** Errors logged after the entry `seenId` (the last one on screen when the drawer was open): the badge of the Log button. */
export const unseenErrors = (entries: LogEntry[], seenId: number): number =>
  entries.filter((e) => e.level === 'error' && e.id > seenId).length;

/**
 * The drawer opens by itself for a failed reload, a failed build, and the app's own emulator errors (a guest fault, stall or DSP halt). Of the
 * runtime logs (origin `emulator` and `device`) only the `fault: …` entry the follower makes from dk/fault opens it; an error row does not
 * (it counts in the badge of the Log button). The follower makes each fault once, so a closed drawer stays closed until a new fault.
 */
export const autoOpens = (e: Pick<LogInput, 'source' | 'level' | 'text' | 'rt'>): boolean => {
  if (e.level !== 'error') return false;
  if (e.rt) return !e.rt.event && e.text.startsWith('fault:');
  return e.source === 'reload' || e.source === 'build' || e.source === 'emulator';
};

const two = (n: number) => String(n).padStart(2, '0');
/** HH:MM:SS in the local time. */
export function formatTime(at: number): string {
  const d = new Date(at);
  return `${two(d.getHours())}:${two(d.getMinutes())}:${two(d.getSeconds())}`;
}

const TIME_WIDTH = 8;
const SOURCE_WIDTH = 8;
/** One entry as text; a detail follows on indented lines. */
export function entryText(e: LogEntry): string {
  const mark = e.level === 'error' ? ' ✕' : '';
  const head = `${formatTime(e.at)}  ${(e.source + mark).padEnd(SOURCE_WIDTH + 2)}${e.text}`;
  if (!e.detail) return head;
  const indent = ' '.repeat(TIME_WIDTH + 2);
  return `${head}\n${e.detail
    .trimEnd()
    .split('\n')
    .map((l) => indent + l)
    .join('\n')}`;
}

/** The text that Copy puts on the clipboard: the entries given (the visible ones), details included. */
export const copyText = (entries: LogEntry[]): string => entries.map(entryText).join('\n');

// ----- the file that Save writes -----

/** The local time as `2026-10-30T14:03:22.123` (what `dklog follow` writes as `host_time`); without the milliseconds when `withMs` is false. */
function isoLocal(at: number, withMs: boolean): string {
  const d = new Date(at);
  const day = `${d.getFullYear()}-${two(d.getMonth() + 1)}-${two(d.getDate())}`;
  const time = `${two(d.getHours())}:${two(d.getMinutes())}:${two(d.getSeconds())}`;
  return `${day}T${time}${withMs ? `.${String(d.getMilliseconds()).padStart(3, '0')}` : ''}`;
}

/** The suggested file name: `device-log-2026-10-30-1403.log`. */
export function saveName(now: number): string {
  const d = new Date(now);
  return `device-log-${isoLocal(now, false).slice(0, 10)}-${two(d.getHours())}${two(d.getMinutes())}.log`;
}

/**
 * The file Save writes, in the format of `tools/device/dklog.py follow`: a `# ` line with the readable text of each entry, and after it, for an
 * event of a runtime log, the event as one JSON object on a line of its own (the `event_dict` fields and `host_time`). `grep -v '^#'` gives
 * JSON lines. Entries of the app and notes are `# ` lines only (a detail follows on `# ` lines).
 */
export function saveText(entries: LogEntry[], now: number): string {
  const lines = [`# workbench log, saved ${isoLocal(now, false)}`];
  for (const e of entries) {
    if (e.rt?.event) {
      lines.push(`# ${e.rt.line ?? e.text}`, JSON.stringify({ ...e.rt.event, host_time: isoLocal(e.at, true) }));
      continue;
    }
    const mark = e.level === 'error' ? ' ✕' : '';
    lines.push(`# ${formatTime(e.at)}  ${e.source}${mark}  ${e.text}`);
    for (const l of (e.detail ?? '').trimEnd().split('\n')) if (l) lines.push(`#   ${l}`);
  }
  return `${lines.join('\n')}\n`;
}

// ----- the texts of the events -----

/** "248 ms", "2.3 s". */
export const duration = (ms: number): string => (ms < 1000 ? `${Math.round(ms)} ms` : `${(ms / 1000).toFixed(1)} s`);

/** A reload that worked. */
export function reloadEntry(report: ReloadReport): LogInput {
  const built = report.build?.built ?? [];
  const parts: string[] = [];
  if (built.length) parts.push(`built ${built.map((b) => b.id).join(', ')} (${duration(report.build?.ms ?? 0)})`);
  const took =
    report.ms !== undefined
      ? ` (${duration(report.ms)})`
      : report.seconds !== undefined
        ? ` (${duration(report.seconds * 1000)})`
        : '';
  switch (report.path) {
    case 'swap':
      parts.push(`swapped ${report.bytes ?? 0} B in ${report.owners?.join(', ') || 'no module'}${took}`);
      break;
    case 'none':
      parts.push(`nothing changed${took}`);
      break;
    case 'dsp':
      parts.push(`DSP only, injected${took}`);
      break;
    default:
      parts.push(`booted the new image, ${report.reason ?? 'the core changed'}${took}`);
  }
  return { source: 'reload', level: 'info', text: parts.join(' · ') };
}

/** A reload that failed: `failure` is `reloadError(e)`. A build error is a `build` entry, the compiler output its detail. */
export function reloadFailureEntry(failure: { status: string; full: string }): LogInput {
  const text = failure.status.replace(/^Reload failed: /, '');
  const build = text.startsWith('build of');
  return {
    source: build ? 'build' : 'reload',
    level: 'error',
    text: build ? text : `failed: ${text}`,
    ...(failure.full ? { detail: failure.full.replace(/^Reload failed: /, '') } : {}),
  };
}

export interface FirmwareLoad {
  /** The file name. */
  name: string;
  device: string;
  version: string;
  /** Whether the load asked for the digikit runtime, and what the controller reported of it. */
  runtime: boolean;
  patch?: { patched: boolean; set?: string; cache_hit?: boolean } | null;
  ms: number;
}

/** A firmware load or boot that worked. */
export function firmwareEntry(f: FirmwareLoad): LogInput {
  let runtime = 'no runtime';
  if (f.patch?.patched)
    runtime = `runtime ${f.patch.set ?? ''}${f.patch.cache_hit ? ' (cached image)' : ' (patched now)'}`.replace(
      '  ',
      ' ',
    );
  else if (f.patch) runtime = 'patched image, loaded as it is';
  else if (f.runtime) runtime = 'runtime requested, not applied';
  return {
    source: 'firmware',
    level: 'info',
    text: `loaded ${f.name} · ${f.device} ${f.version} · ${runtime} · boot ${duration(f.ms)}`,
  };
}

/** One log entry for the Firmware page: a flash, a restore or a saved image (the result in the text, the end message as the detail). */
export const flashEntry = (run: FirmwareRun): LogInput => ({
  source: 'firmware',
  level: run.level,
  text: run.text,
  ...(run.detail ? { detail: run.detail } : {}),
});

export const failureEntry = (source: LogSource, what: string, error: string): LogInput => ({
  source,
  level: 'error',
  text: `${what} failed: ${error.split('\n')[0]}`,
  ...(error.includes('\n') ? { detail: error } : {}),
});

/** A card sync or folder change. */
export function cardEntry(what: string, c: CardFolder): LogInput {
  if (c.error) return { source: 'card', level: 'error', text: `${what}: ${c.error}` };
  const n = c.files.length;
  const where = c.folder ? shortFolder(c.folder) : 'no folder';
  const later = c.reload_needed ? ' · reload to use it' : '';
  return { source: 'card', level: 'info', text: `${what}: ${where} (${n} file${n === 1 ? '' : 's'})${later}` };
}

const PAST: Record<string, string> = {
  Installing: 'installed',
  Restarting: 'restarted',
  Uploading: 'uploaded',
  Deleting: 'deleted',
  Creating: 'created a folder',
};

/** One log entry for a run of the Transfer page: files sent and skipped, the end text; the event lines are the detail. */
export function transferEntry(run: TransferRun): LogInput {
  const detail = run.lines.length ? { detail: run.lines.join('\n') } : {};
  if (run.cancelled)
    return {
      source: 'transfer',
      level: 'info',
      text: `${run.what.toLowerCase()} cancelled${run.counts ? `: ${run.counts}` : ''}`,
      ...detail,
    };
  if (run.error !== undefined) {
    // a run that already words its error as "<What> failed: ..." is not prefixed twice
    const first = run.error.split('\n')[0] ?? '';
    const own = `${run.what} failed: `;
    return {
      source: 'transfer',
      level: 'error',
      text: first.startsWith(own)
        ? run.what.toLowerCase() + first.slice(run.what.length)
        : `${run.what.toLowerCase()} failed: ${first}`,
      ...detail,
    };
  }
  // an upload run of the Samples mode: one line per file (`a: verified`)
  const files =
    run.what === 'Uploading' ? run.lines.filter((l) => /: (verified|not sent|failed|not finished)/.test(l)) : [];
  if (files.length) {
    const n = files.filter((l) => l.endsWith(': verified')).length;
    const text = `${PAST.Uploading} ${n} file${n === 1 ? '' : 's'} on ${run.target}${n === files.length ? ', verified' : ''}`;
    return { source: 'transfer', level: n === files.length ? 'info' : 'error', text, ...detail };
  }
  const sent = run.lines.filter((l) => /^Uploading .* bytes\)$/.test(l)).length;
  const skipped = run.lines.filter((l) => /already on the card/.test(l)).length;
  const counts = [sent ? `sent ${sent}` : '', skipped ? `skipped ${skipped}` : ''].filter(Boolean).join(', ');
  const text = [`${PAST[run.what] ?? run.what.toLowerCase()} on ${run.target}`, counts, run.message ?? '']
    .filter(Boolean)
    .join(' · ');
  return { source: 'transfer', level: 'info', text, ...detail };
}

// ----- emulator health -----

/** What the app last saw of the guest's health. */
export interface Health {
  error: string | null;
  stalled: boolean;
  halted: string | null;
}
export const healthStart = (): Health => ({ error: null, stalled: false, halted: null });

/** A health reading; a field that is left out was not read this time. */
export interface HealthReading {
  error?: string | null;
  stalled?: boolean;
  halted?: string | null;
  /** The stall threshold in emulated ms. */
  stallMs?: number;
}

/** The entries for what changed since `prev`, and the new state. A fault, stall or halt is logged once, when it starts. */
export function healthChanges(prev: Health, now: HealthReading): { health: Health; entries: LogInput[] } {
  const health = { ...prev };
  const entries: LogInput[] = [];
  const error = (text: string) => entries.push({ source: 'emulator', level: 'error', text });
  if (now.error !== undefined) {
    const e = now.error || null;
    if (e && e !== prev.error) error(`fault: ${e}`);
    health.error = e;
  }
  if (now.stalled !== undefined) {
    if (now.stalled && !prev.stalled)
      error(`stall: the main loop stopped${now.stallMs ? ` for more than ${duration(now.stallMs)}` : ''}`);
    if (!now.stalled && prev.stalled) entries.push({ source: 'emulator', level: 'info', text: 'stall ended' });
    health.stalled = now.stalled;
  }
  if (now.halted !== undefined) {
    const h = now.halted || null;
    if (h && h !== prev.halted) error(`DSP halt: ${h}`);
    health.halted = h;
  }
  return { health, entries };
}
