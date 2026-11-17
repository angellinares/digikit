// Following the runtime log of one origin (the emulator or a device): the pure part, with no timers and no Tauri. One `tick` asks the origin
// for the events it has not seen, and the follower says what it found as log entries. The rules and texts are those of `dklog.py follow`
// (`Follower.poll`); the Source menu, the timers and the sessions are in log-follow.ts. Host tests: apps/desktop/ui/tests/log-follow.test.ts.
import {
  faultDetail,
  faultSummary,
  eventDetail,
  eventLine,
  eventText,
  FAULT_DETAIL_KINDS,
  moduleOf,
  runtimeEvent,
  sameFault,
  type CardLogState,
  type DkLogEvent,
  type Fault,
  type LogConfig,
  LogCursor,
} from '@digi/dk-session';
import { CAP, LOG_KIND, LOG_LEVELS } from '@digi/dk-session/gen_proto';
import type { LogInput, Origin } from './log-core.ts';
import type { LogNames } from '@digi/dk-session/log-names';
import { hasLogcfg } from './log-level-core.ts';

/** What the follower needs of a protocol session (a `DkSession` with `followSession`, or a fake). */
export interface FollowSession {
  /** INFO: the capabilities, and the port name and image id the names need. */
  info(quick?: boolean): Promise<{ caps: number | null; image_id?: string | null; port?: string }>;
  /** PEEK of the debug module (`DkSession.peek`): the load table for the names. */
  peek?(addr: number, len: number): Promise<Uint8Array>;
  logPage(from: number, quick?: boolean): Promise<[number, number, DkLogEvent[]]>;
  logConfig(quick?: boolean): Promise<LogConfig>;
  /** LOGCFG set (the Level menu of the Log drawer). */
  setLogConfig(level: number, modules: number[], quick?: boolean): Promise<LogConfig>;
  /** LOGCFG op 2: the card log state, or null when the debug module has no card log. */
  cardLog?(quick?: boolean): Promise<CardLogState | null>;
  /** LOGCFG op 3 (the Card log button of the Log drawer). */
  setCardLog?(on: boolean, quick?: boolean): Promise<CardLogState>;
  /** The loaded modules; `index` is the module record index. */
  modules(): Promise<{ index: number; name: string }[]>;
  /** The fault record the runtime keeps on the card (`dk/fault`), or null. */
  fault(): Promise<Fault | null>;
}

/** What the Source menu and the Follow button show of an origin. */
export type OriginStatus =
  /** Not followed (Follow is off, or nothing runs). */
  | 'off'
  | 'following'
  /** The origin answers INFO but has no runtime (an old image, or no digikit). */
  | 'no-runtime'
  /** The runtime answers but the debug module (`digikit/debug`) is not loaded: no LOG, only the fault record. */
  | 'no-debug'
  /** The MIDI port is not there. */
  | 'absent'
  | 'frozen';

export interface FollowOut {
  push(inputs: LogInput[]): void;
  /** The names of the modules the origin has loaded changed. */
  modules(names: string[]): void;
  status(status: OriginStatus): void;
  /** The LOGCFG state the follower read (at the start and after a restart). */
  config?(config: LogConfig): void;
  /** Whether the origin's INFO has the LOGCFG capability (the Level menu works on it). */
  logcfg?(supported: boolean): void;
  /** The card log state read with every round; null when the debug module has no card log. */
  cardlog?(state: CardLogState | null): void;
}

export interface FollowOptions {
  origin: Origin;
  session(): Promise<FollowSession>;
  /** Is the port (or the emulator) there? Asked when a request fails: a missing port is not a frozen device. */
  present?(): Promise<boolean>;
  now(): number;
  out: FollowOut;
  /** Names for the addresses of crashes and faults, read through PEEK of the debug module: the same path for the emulator and a device. */
  names?: LogNames;
}

/** No answer for this long: the device may be frozen. */
export const FROZEN_MS = 5000;
export const POLL_MS = 1000;
/** Is the window on screen? A hidden window (minimized, on another desktop) asks nothing of the device. */
export const windowShown = (): boolean => typeof document === 'undefined' || document.visibilityState !== 'hidden';
/** The device is read this often while the Log drawer is closed (the badge and the auto-open for errors still work; the ring holds the events). */
export const IDLE_MS = 5000;

/**
 * How a loop paces itself. A hidden window asks nothing (`ask` false; it looks again after `POLL_MS`). The device on the MIDI port is read
 * at most every `IDLE_MS` while the drawer is closed; the emulator is a local call and keeps its pace.
 */
export function pace(
  device: boolean,
  shown: boolean,
  drawerOpen: boolean,
  wait: number,
): { ask: boolean; wait: number } {
  if (!shown) return { ask: false, wait: POLL_MS };
  return { ask: true, wait: device && !drawerOpen ? Math.max(wait, IDLE_MS) : wait };
}
/** The events of the current boot that are kept for the detail of a fault. */
const KEEP = 1100;

/** The events whose text has an address to name. */
const NAMED_KINDS: ReadonlySet<number> = new Set([LOG_KIND.CRASH_FOUND, LOG_KIND.FAULT_FOUND]);

const isCancelled = (e: unknown): boolean => (e as { kind?: string } | null)?.kind === 'cancelled';
const hasLog = (caps: number | null): boolean => caps !== null && ((caps >>> CAP.LOG) & 1) === 1;
/** A runtime answers (INFO carries capabilities, so it is a digikit runtime of this protocol), whether or not the debug module is loaded. */
const hasRuntime = (caps: number | null): boolean => caps !== null && ((caps >>> CAP.SERVICE_CMDS) & 1) === 1;

export class LogFollower {
  /** Whether the follower waits for a firmware flash to end. */
  paused = false;
  private page: FollowSession | null = null;
  private cursor = new LogCursor({ logPage: (from) => this.page!.logPage(from, true) });
  private seenNewest = 0;
  private names = new Map<number, string>();
  private boot: DkLogEvent[] = [];
  private everStarted = false;
  private started = false;
  private lastAnswer: number;
  private frozen = false;
  private faultDue = false;
  private configDue = false;
  private modulesDue = false;
  /** The debug module has no card log (or no LOGCFG): not asked again until the next start. */
  private noCardLog = false;
  private lastConfig = '';
  private lastFault: Fault | null = null;
  /** The runtime answers but has no debug module: only the fault record is read. */
  private noDebug = false;
  private listed = '';
  /** The maps and the load table are read again before the next entry that names an address: at the start, and when the module list changes. */
  private namesDue = true;
  private lastNamesNote = '';

  constructor(private o: FollowOptions) {
    this.lastAnswer = o.now();
  }

  /** One round. Returns how long to wait before the next one. */
  async tick(): Promise<number> {
    try {
      const s = await this.o.session();
      if (!this.started && !(await this.start(s))) return this.noDebug ? IDLE_MS : FROZEN_MS;
      await this.poll(s);
      this.answered();
    } catch (e) {
      return this.failed(e);
    }
    return POLL_MS;
  }

  /** A firmware flash starts: the port belongs to the Firmware page. */
  pause(): void {
    if (this.paused) return;
    this.paused = true;
    this.say('paused during the firmware flash');
    this.o.out.status('off');
  }

  /** The flash is over: the device restarts, so its first answers may take a while. */
  resume(): void {
    this.paused = false;
    this.started = false;
    this.lastAnswer = this.o.now();
  }

  private say(text: string, level: 'info' | 'error' = 'info', detail?: string): void {
    this.o.out.push([
      { source: this.o.origin, level, text, ...(detail ? { detail } : {}), rt: { origin: this.o.origin, module: '' } },
    ]);
  }

  /** INFO decides whether there is a runtime log to follow: it has the LOG capability while the debug module is loaded. */
  private async start(s: FollowSession): Promise<boolean> {
    const info = await s.info(true);
    if (!hasLog(info.caps)) {
      this.noDebug = hasRuntime(info.caps);
      this.o.out.status(this.noDebug ? 'no-debug' : 'no-runtime');
      this.o.out.logcfg?.(false);
      this.lastAnswer = this.o.now();
      // the fault record is on the card whether or not the module is loaded
      if (this.noDebug) await this.readFault(s);
      return false;
    }
    this.noDebug = false;
    this.started = true;
    this.o.out.logcfg?.(hasLogcfg(info.caps));
    this.o.out.status('following');
    this.lastAnswer = this.o.now();
    if (!this.everStarted) {
      this.everStarted = true;
      this.faultDue = true;
    }
    this.configDue = true;
    this.modulesDue = true;
    this.noCardLog = !hasLogcfg(info.caps);
    if (this.noCardLog) this.o.out.cardlog?.(null);
    return true;
  }

  private answered(): void {
    if (this.frozen) {
      this.frozen = false;
      this.say('the device answers again');
      this.o.out.status('following');
    }
    this.lastAnswer = this.o.now();
  }

  private async failed(e: unknown): Promise<number> {
    if (isCancelled(e)) return POLL_MS;
    if (this.o.present && !(await this.o.present().catch(() => true))) {
      this.started = false;
      this.lastAnswer = this.o.now();
      this.o.out.status('absent');
      return FROZEN_MS;
    }
    if (!this.frozen && this.o.now() - this.lastAnswer >= FROZEN_MS) {
      this.frozen = true;
      this.say('no answer to LOG for 5 s: the device may be frozen (still asking)', 'info');
      this.o.out.status('frozen');
    }
    return this.frozen ? FROZEN_MS : POLL_MS;
  }

  /** Read what the origin has from `next` on; the rules of `Follower.poll`. */
  private async poll(s: FollowSession): Promise<void> {
    this.page = s;
    // The cursor anchors on the last event it returned: it finds a restart (also when the new log is as long as the old position) and a gap.
    const { events, newBoot, lost } = await this.cursor.read();
    const rows: LogInput[] = [];
    if (newBoot) {
      const now = events.at(-1)?.seq ?? 0;
      this.say(`the ${this.o.origin} restarted: the log starts again (was at ${this.seenNewest}, now ${now})`);
      this.seenNewest = 0;
      this.boot = [];
      this.faultDue = this.configDue = this.modulesDue = this.namesDue = true;
    }
    if (events.length > 0) {
      if (events.some((e) => NAMED_KINDS.has(e.kind))) await this.refreshNames(s);
      if (lost > 0) rows.push(this.note(`${lost} events lost (the ring held from ${events[0]!.seq})`));
      for (const ev of events) this.take(ev, rows);
      this.seenNewest = Math.max(this.seenNewest, events[events.length - 1]!.seq);
      this.o.out.push(rows);
    }
    if (this.modulesDue) await this.readModules(s);
    if (this.configDue) await this.readConfig(s);
    if (this.faultDue) await this.readFault(s);
    if (!this.noCardLog) await this.readCardLog(s);
  }

  /** With every round: the write count and the age of the last write change while the log runs. */
  private async readCardLog(s: FollowSession): Promise<void> {
    if (!s.cardLog) return;
    const state = await this.aux(() => s.cardLog!(true));
    if (state === undefined) return;
    this.noCardLog = state === null;
    this.o.out.cardlog?.(state);
  }

  private note(text: string): LogInput {
    return { source: this.o.origin, level: 'info', text, rt: { origin: this.o.origin, module: '' } };
  }

  private take(ev: DkLogEvent, rows: LogInput[]): void {
    if (ev.kind === LOG_KIND.MOD_LIST) {
      this.names.set(ev.args[0]! + 1, runtimeEvent(ev).fields.name as string);
      this.modulesDue = true;
    } else if (ev.kind === LOG_KIND.MOD_LOAD) this.modulesDue = true;
    this.boot.push(ev);
    if (this.boot.length > KEEP) this.boot.splice(0, this.boot.length - KEEP);
    if (FAULT_DETAIL_KINDS.has(ev.kind)) return; // part of the fault entry's detail
    const d = runtimeEvent(ev);
    rows.push({
      source: this.o.origin,
      level: d.error ? 'error' : 'info',
      text: eventText(d, this.names, this.o.names?.resolve()),
      detail: eventDetail(d),
      rt: {
        origin: this.o.origin,
        module: moduleOf(d.source, this.names),
        event: d,
        line: eventLine(d, this.names, this.o.names?.resolve()),
      },
    });
  }

  /** Read the maps and the load table when they are due. The read is made for a crash or a fault only: it asks the controller to patch the image once. */
  private async refreshNames(s: FollowSession): Promise<void> {
    const names = this.o.names;
    if (!names || !this.namesDue) return;
    this.namesDue = false;
    let why: string | undefined;
    try {
      await names.refresh(s);
      why = names.unavailable();
    } catch (e) {
      if (isCancelled(e)) throw e;
      why = `the load table could not be read (${e instanceof Error ? e.message : String(e)})`;
    }
    if (why && why !== this.lastNamesNote) this.say(`${this.o.origin}: names unavailable: ${why}`);
    this.lastNamesNote = why ?? '';
  }

  /** Keep the call from stopping the round: a runtime without the command is a runtime without it. */
  private async aux<T>(call: () => Promise<T>): Promise<T | undefined> {
    try {
      return await call();
    } catch (e) {
      if (isCancelled(e)) throw e;
      return undefined;
    }
  }

  private async readModules(s: FollowSession): Promise<void> {
    this.modulesDue = false;
    const list = await this.aux(() => s.modules());
    if (list) for (const m of list) this.names.set(m.index + 1, m.name);
    const names = [...this.names].sort(([a], [b]) => a - b).map(([, name]) => name);
    if (names.join('\n') === this.listed) return;
    this.listed = names.join('\n');
    this.namesDue = true; // the modules sit at other addresses now
    this.noCardLog = false; // a debug module loaded later may have the card log
    this.o.out.modules(names);
  }

  private async readConfig(s: FollowSession): Promise<void> {
    this.configDue = false;
    const cfg = await this.aux(() => s.logConfig(true));
    if (!cfg) return;
    this.o.out.config?.(cfg);
    const text = `${this.o.origin}: debug module loaded, level ${LOG_LEVELS[cfg.level] ?? cfg.level}`;
    if (text === this.lastConfig) return;
    this.lastConfig = text;
    this.say(text);
  }

  private async readFault(s: FollowSession): Promise<void> {
    this.faultDue = false;
    const f = await this.aux(() => s.fault());
    if (!f || sameFault(f, this.lastFault)) return;
    this.lastFault = f;
    await this.refreshNames(s);
    this.say(`fault: ${faultSummary(f)}`, 'error', faultDetail(f, this.boot, this.names, this.o.names?.resolve()));
  }
}
