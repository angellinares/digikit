// Wait for events of the runtime's log (LOG, docs/findings/46-digikit-debug.md) instead of a fixed time. `Events.until(kind, predicate, {emulatedMs})`
// reads the log from a cursor, so one event matches one wait. Only sequence numbers order events: on a zero-DSP daemon the block counter
// does not advance (finding 46).
//
// Cursor rules:
// - `listen()` puts the cursor at the newest event: older events never match.
// - A wait consumes events up to the one that matched; the next wait starts after it.
// - A new boot restarts the log at sequence 1. The cursor keeps the last consumed event and reads it again with each poll: when it is gone or
//   differs (or the log is shorter), the boot changed, the cursor starts at the new log's first event and `boots` counts it.
// - A gap (the oldest kept event is newer than the cursor: the ring dropped events we had not read) is an error, never a silent miss.
// - A missing event fails after `emulatedMs` of emulated time with the last events seen.
//
// The log level of events such as INPUT_TAKEN is debug (3); `listen()` raises the level of the named module records with LOGCFG and leaves the default.
import { setTimeout as delay } from 'node:timers/promises';
import type { Session } from './session.ts';

/** The shape of a LOG event (`DkLogEvent` of @digi/dk-session). */
export interface LogEvent {
  seq: number;
  time: number;
  kind: number;
  flags: number;
  args: number[];
}

/** The calls of a debug session that the helper needs (`DkSession` of @digi/dk-session has them). */
export interface LogReader {
  logPage(from: number): Promise<[oldest: number, newest: number, events: LogEvent[]]>;
  logConfig(): Promise<{ level: number; modules: number[] }>;
  setLogConfig(level: number, modules: number[]): Promise<unknown>;
  modules(): Promise<{ index: number; name: string }[]>;
}

export interface EventsOptions {
  /** The BOOT_START kind: an event of it after the cursor means a new boot even if the numbers fit. */
  bootStart?: number;
  /** Kind names for failure messages. */
  name?: (kind: number) => string;
  /** Module names whose records `listen()` sets to debug (3). */
  debugModules?: string[];
  /** The level `listen()` sets, default 3. */
  level?: number;
  /** Real milliseconds between polls, default 50. */
  pollMs?: number;
  /** Fail when the emulated clock does not move for this long (real milliseconds), default 30 s. */
  hangMs?: number;
  /** How many events a failure message shows, default 12. */
  tail?: number;
}

/** ColdFire instructions per emulated millisecond (the input clock of `panel`). */
const INSNS_PER_MS = 132_000;
const NEWEST = 0xffffffff;

export class Events {
  /** The last consumed event, or null before the first. The cursor is its sequence number plus 1. */
  private last: LogEvent | null = null;
  private recent: LogEvent[] = [];
  private maxSeen = 0;
  /** New boots seen since `listen()`. */
  boots = 0;
  private readonly name: (kind: number) => string;
  private readonly tail: number;
  private readonly pollMs: number;
  private readonly hangMs: number;

  private readonly log: LogReader;
  private readonly session: Pick<Session, 'status'>;
  private readonly options: EventsOptions;

  constructor(log: LogReader, session: Pick<Session, 'status'>, options: EventsOptions = {}) {
    this.log = log;
    this.session = session;
    this.options = options;
    this.name = options.name ?? ((k) => `kind ${k}`);
    this.tail = options.tail ?? 12;
    this.pollMs = options.pollMs ?? 50;
    this.hangMs = options.hangMs ?? 30_000;
  }

  /** Start at the newest event and set the levels. Call it once the debug module is loaded (and again after a restart that loses the levels). */
  async listen(): Promise<void> {
    const [, newest] = await this.log.logPage(NEWEST);
    this.last = null;
    this.recent = [];
    this.maxSeen = newest;
    if (newest > 0) {
      const [, , events] = await this.log.logPage(newest);
      this.last = events.find((e) => e.seq === newest) ?? null;
    }
    await this.setLevels();
  }

  /** The records of `debugModules` that are loaded go to level `level`; the other levels stay. */
  async setLevels(): Promise<void> {
    const want = new Set(this.options.debugModules ?? ['digikit/remote', 'digikit/debug']);
    const config = await this.log.logConfig();
    const modules = [...config.modules];
    for (const m of await this.log.modules())
      if (want.has(m.name) && m.index < modules.length) modules[m.index] = this.options.level ?? 3;
    await this.log.setLogConfig(config.level, modules);
  }

  /** The events seen so far, one per line (the end of a failure message). */
  seen(): string {
    if (this.recent.length === 0) return '(no events seen)';
    return this.recent
      .slice(-this.tail)
      .map((e) => `#${e.seq} ${this.name(e.kind)} ${e.args.map((a) => (a | 0).toString()).join(' ')}`)
      .join('\n');
  }

  /**
   * Wait for the next event of `kind` that satisfies `predicate` and return it. `emulatedMs` is emulated time, from the daemon's instruction count.
   * Events before the cursor never match; the ones read on the way are consumed with it.
   */
  async until(
    kind: number,
    predicate: (event: LogEvent) => boolean,
    { emulatedMs }: { emulatedMs: number },
  ): Promise<LogEvent> {
    const start = await this.clock();
    const target = start + emulatedMs * INSNS_PER_MS;
    let seen = start;
    let lastAdvance = Date.now();
    for (;;) {
      const hit = await this.poll(kind, predicate);
      if (hit) return hit;
      if (seen >= target) break;
      await delay(this.pollMs);
      const now = await this.clock();
      if (now > seen) lastAdvance = Date.now();
      else if (Date.now() - lastAdvance >= this.hangMs) {
        throw new Error(
          `events.until ${this.name(kind)}: the emulated clock is stuck at ${now} for ${this.hangMs} ms\n${this.seen()}`,
        );
      }
      seen = now;
    }
    throw new Error(
      `events.until ${this.name(kind)}: not seen within ${emulatedMs} ms of emulated time. Last events:\n${this.seen()}`,
    );
  }

  private async clock(): Promise<number> {
    const elapsed = (await this.session.status()).elapsed;
    if (typeof elapsed !== 'number') throw new Error('events needs status.elapsed');
    return elapsed;
  }

  /** Read what is new; return the first event that matches, with the cursor after it. */
  private async poll(kind: number, predicate: (event: LogEvent) => boolean): Promise<LogEvent | null> {
    let from = this.last ? this.last.seq : 0;
    let anchored = this.last !== null;
    for (;;) {
      const [oldest, newest, events] = await this.log.logPage(from);
      let batch = events;
      if (anchored) {
        // the first event of the page is the last consumed one, unless the log is another boot's or lost it
        const head = batch[0];
        const last = this.last!;
        const sameBoot = head !== undefined && head.seq === last.seq && sameEvent(head, last);
        if (!sameBoot && newest >= last.seq && oldest > last.seq) {
          throw new Error(
            `events: the log lost events (oldest kept #${oldest}, the cursor is at #${last.seq + 1})\n${this.seen()}`,
          );
        }
        batch = sameBoot ? batch.slice(1) : [];
        if (!sameBoot || batch.some((e) => e.kind === this.options.bootStart)) {
          await this.newBoot();
          from = 0;
          anchored = false;
          continue;
        }
        anchored = false;
      }
      for (const e of batch) {
        if (e.seq > this.maxSeen) {
          this.recent.push(e);
          this.maxSeen = e.seq;
          if (this.recent.length > 4 * this.tail) this.recent.shift();
        }
        this.last = e;
        if (e.kind === kind && predicate(e)) return e;
      }
      if (batch.length === 0 || this.last === null || this.last.seq >= newest) return null;
      from = this.last.seq + 1;
    }
  }

  /** The log starts again: the cursor goes to the new log's first event, and the levels (RAM only) are set again when the modules are there. */
  private async newBoot(): Promise<void> {
    this.boots++;
    this.last = null;
    this.recent = [];
    this.maxSeen = 0;
    await this.setLevels().catch(() => undefined); // a module not registered yet: call listen() later to set them
  }
}

function sameEvent(a: LogEvent, b: LogEvent): boolean {
  return a.time === b.time && a.kind === b.kind && a.args.every((v, i) => v === b.args[i]);
}
