// The log levels of an origin (the Level menu of the Log drawer), the pure part: reading LOGCFG, the rows of the menu, and a change as the
// 10-byte set request the runtime takes (default level, 8 module records, 0xff = the default). No DOM and no timers. Host tests: apps/desktop/ui/tests/log-level.test.ts.
import type { DkModule, LogConfig } from '@digi/dk-session';
import { CAP, LOG_LEVELS } from '@digi/dk-session/gen_proto';
import type { Origin } from './log-core.ts';

/** The level a module record has when it follows the default. */
export const USE_DEFAULT = 0xff;
/** The runtime keeps 8 module records (LOGCFG). */
export const MODULE_RECORDS = 8;

/** What the Level menu needs of a protocol session (a `DkSession`, or a fake). */
export interface LevelSession {
  info(quick?: boolean): Promise<{ caps: number | null }>;
  logConfig(quick?: boolean): Promise<LogConfig>;
  setLogConfig(level: number, modules: number[], quick?: boolean): Promise<LogConfig>;
  modules(): Promise<Pick<DkModule, 'index' | 'name'>[]>;
}

export interface LevelModule {
  /** The module record, 0 to 7. */
  index: number;
  name: string;
  /** 0 to 4, or `USE_DEFAULT`. */
  level: number;
}

/** What an origin's log levels are now. */
export interface LevelView {
  level: number;
  modules: LevelModule[];
  /** The debug module is loaded (it always answers 1). */
  debug: boolean;
}

export const hasLogcfg = (caps: number | null): boolean => caps !== null && ((caps >>> CAP.LOGCFG) & 1) === 1;

const viewOf = (cfg: LogConfig, modules: Pick<DkModule, 'index' | 'name'>[]): LevelView => ({
  level: cfg.level,
  debug: cfg.debug,
  modules: modules
    .filter((m) => m.index >= 0 && m.index < MODULE_RECORDS)
    .sort((a, b) => a.index - b.index)
    .map((m) => ({ index: m.index, name: m.name, level: cfg.modules[m.index] ?? USE_DEFAULT })),
});

/** The LOGCFG state a view shows (the shape the follower reports). */
export const configOf = (v: LevelView): LogConfig => {
  const modules = Array<number>(MODULE_RECORDS).fill(USE_DEFAULT);
  for (const m of v.modules) modules[m.index] = m.level;
  return { level: v.level, modules, debug: v.debug };
};

/** The levels of the origin, or null when its INFO has no LOGCFG capability. */
export async function readLevels(s: LevelSession): Promise<LevelView | null> {
  if (!hasLogcfg((await s.info(true)).caps)) return null;
  const cfg = await s.logConfig(true);
  return viewOf(cfg, await s.modules().catch(() => []));
}

/** A change from the menu: the default level, or one module record's level (or `USE_DEFAULT`). */
export type LevelChange = { what: 'default'; level: number } | { what: 'module'; index: number; level: number };

/** The request that makes `change` from the levels `cfg` has now: the other records stay as they are. */
export function requestFor(cfg: LogConfig, change: LevelChange): { level: number; modules: number[] } {
  const modules = Array.from({ length: MODULE_RECORDS }, (_, i) => cfg.modules[i] ?? USE_DEFAULT);
  if (change.what === 'default') return { level: change.level, modules };
  modules[change.index] = change.level;
  return { level: cfg.level, modules };
}

/** Make the change, then read LOGCFG again: the view is what the runtime says, not what was asked. */
export async function changeLevel(s: LevelSession, change: LevelChange): Promise<LevelView> {
  if (!hasLogcfg((await s.info(true)).caps))
    throw new Error('This target has no log level control (the debug module is not loaded).');
  const now = await s.logConfig(true);
  const { level, modules } = requestFor(now, change);
  await s.setLogConfig(level, modules);
  return viewOf(await s.logConfig(true), await s.modules().catch(() => []));
}

export const levelName = (level: number): string => LOG_LEVELS[level] ?? String(level);

/** The text of the Level button: `Level: info`, with the number of modules that have a level of their own. */
export function levelLabel(level: number | undefined, modules: number[] = []): string {
  if (level === undefined) return 'Level';
  const own = modules.filter((m) => m !== USE_DEFAULT).length;
  return `Level: ${levelName(level)}${own ? ` +${own}` : ''}`;
}

/** The Level buttons of a row: the default row has the five levels, a module row has `default` first. */
export function levelChoices(row: 'default' | 'module'): { value: number; label: string }[] {
  const levels = LOG_LEVELS.map((label, value) => ({ value, label }));
  return row === 'module' ? [{ value: USE_DEFAULT, label: 'default' }, ...levels] : levels;
}

/** Why the controls are off, or '' when they are on. */
export function levelsOff(o: { origin: string | undefined; live: boolean; supported: boolean | undefined }): string {
  if (!o.origin) return 'Choose Emulator or Device in Source: the levels are set on one of them';
  if (!o.live) return `The ${o.origin} is not followed: nothing answers for its log levels`;
  if (o.supported === false) return `The debug module is not loaded on the ${o.origin}: it has no log level control`;
  return '';
}

/**
 * The origin the Level menu acts on: the one chosen in Source (`emulator:…`, `device:…`, and `emulator` for the app's entries about it). For
 * All and the app's sources it is the only origin that is followed, and none when both or neither are.
 */
export function levelOrigin(source: string, live: Record<Origin, boolean>): Origin | undefined {
  const head = source.split(':')[0];
  if (head === 'emulator' || head === 'device') return head;
  if (live.emulator !== live.device) return live.emulator ? 'emulator' : 'device';
  return undefined;
}
