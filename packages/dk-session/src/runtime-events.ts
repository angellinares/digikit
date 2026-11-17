// Waits on the runtime's log (LOG) instead of fixed times, for any device: a cursor over the log pages (`LogCursor`) and the waits on it (`RuntimeEvents`).
//
// Cursor rules:
// - The cursor keeps the last event it returned (the anchor) and asks from the anchor's sequence number: the first event of the page must be the anchor
//   again (same seq, time, kind and arguments). Only sequence numbers order events.
// - A new boot restarts the log at sequence 1: the anchor is missing or differs, a BOOT_START follows it, or the log is shorter than the anchor. The cursor
//   then starts at the new boot's first event and `boots` counts it.
// - A gap (the oldest kept event is newer than the anchor in the same boot: the ring dropped events we had not read) is reported as `lost`, never thrown.
import { LOG_KIND } from './gen_proto.ts';
import type { DkLogEvent, DkSession, Quick } from './dkupload-core.ts';
import { runtimeEvent, type RuntimeEvent } from './runtime-log.ts';

export type LogKindName = keyof typeof LOG_KIND;

/** Time for waits, in ms. `pause` lets about `ms` pass and may throw when time cannot pass (a stuck emulator). */
export interface Clock {
  now(): Promise<number>;
  pause(ms: number): Promise<void>;
}

export const wallClock: Clock = {
  now: async () => Date.now(),
  pause: (ms) => new Promise((r) => setTimeout(r, ms)),
};

/** What one wait reports; the test harness turns it into a timing record. */
export interface WaitRecord {
  kind: 'event' | 'state' | 'time';
  what: string;
  clockMs: number;
  wallMs: number;
  ok: boolean;
}

export interface LogPageSource {
  logPage(from: number, quick?: Quick): Promise<[oldest: number, newest: number, events: DkLogEvent[]]>;
}

const NEWEST = 0xffffffff;

const sameEvent = (a: DkLogEvent, b: DkLogEvent): boolean =>
  a.time === b.time && a.kind === b.kind && a.args.every((v, i) => v === b.args[i]);

export class LogCursor {
  private last: DkLogEvent | null = null;
  private count = 0;
  private readonly src: LogPageSource;
  private readonly probe: <T>(run: (quick: Quick) => Promise<T>) => Promise<T>;

  constructor(src: LogPageSource, opts: { probe?: <T>(run: (quick: Quick) => Promise<T>) => Promise<T> } = {}) {
    this.src = src;
    this.probe = opts.probe ?? (<T>(run: (quick: Quick) => Promise<T>) => run(false));
  }

  get boots(): number {
    return this.count;
  }

  private page(from: number) {
    return this.probe((q) => this.src.logPage(from, q));
  }

  /** Start after the newest event: older events are never returned. */
  async fromNewest(): Promise<void> {
    this.last = null;
    const [, newest] = await this.page(NEWEST);
    if (newest > 0) {
      const [, , events] = await this.page(newest);
      this.last = events.find((e) => e.seq === newest) ?? null;
    }
  }

  /** Start before sequence 1 of the current boot. */
  fromBoot(): void {
    this.last = null;
  }

  /** Everything new since the last read, in order. */
  async read(): Promise<{ events: DkLogEvent[]; newBoot: boolean; lost: number }> {
    let out: DkLogEvent[] = [];
    let newBoot = false;
    let lost = 0;
    let from = this.last ? this.last.seq : 0;
    let anchored = this.last !== null;
    for (;;) {
      const [oldest, newest, events] = await this.page(from);
      let batch = events;
      if (anchored) {
        const anchor = this.last!;
        const head = events[0];
        const same = head !== undefined && head.seq === anchor.seq && sameEvent(head, anchor);
        if (!same && newest >= anchor.seq && oldest > anchor.seq) {
          lost = oldest - (anchor.seq + 1);
          batch = events;
        } else {
          batch = same ? events.slice(1) : [];
          if (!same || batch.some((e) => e.kind === LOG_KIND.BOOT_START) || newest < anchor.seq) {
            this.count++;
            newBoot = true;
            this.last = null;
            out = [];
            lost = 0;
            from = 0;
            anchored = false;
            continue;
          }
        }
        anchored = false;
      }
      out.push(...batch);
      if (batch.length > 0) this.last = batch[batch.length - 1]!;
      if (batch.length === 0 || this.last === null || this.last.seq >= newest) break;
      from = this.last.seq + 1;
    }
    return { events: out, newBoot, lost };
  }
}

export interface RuntimeEventsOptions {
  /** Real or clock milliseconds between polls, default 50. */
  pollMs?: number;
  onWait?: (r: WaitRecord) => void;
  /** Module names whose records `listen()` sets to `level`. */
  debugModules?: string[];
  level?: number;
  /** How many events a failure message shows, default 12. */
  tail?: number;
}

interface WaitOpts {
  withinMs?: number;
}

export class RuntimeEvents {
  private readonly dk: Pick<DkSession, 'logPage' | 'logConfig' | 'setLogConfig' | 'modules'>;
  private readonly clock: Clock;
  private readonly cursor: LogCursor;
  private readonly pollMs: number;
  private readonly onWait: ((r: WaitRecord) => void) | undefined;
  private readonly debugModules: string[];
  private readonly level: number;
  private readonly tail: number;
  private queue: RuntimeEvent[] = [];
  private recent: RuntimeEvent[] = [];

  constructor(
    dk: Pick<DkSession, 'logPage' | 'logConfig' | 'setLogConfig' | 'modules'>,
    clock: Clock,
    opts: RuntimeEventsOptions = {},
  ) {
    this.dk = dk;
    this.clock = clock;
    this.cursor = new LogCursor(dk);
    this.pollMs = opts.pollMs ?? 50;
    this.onWait = opts.onWait;
    this.debugModules = opts.debugModules ?? ['digikit/remote', 'digikit/debug'];
    this.level = opts.level ?? 3;
    this.tail = opts.tail ?? 12;
  }

  get boots(): number {
    return this.cursor.boots;
  }

  /** Cursor at the newest event (or before this boot's first) and the log levels of `debugModules` raised. */
  async listen({ fromBoot = false }: { fromBoot?: boolean } = {}): Promise<void> {
    this.queue = [];
    this.recent = [];
    if (fromBoot) this.cursor.fromBoot();
    else await this.cursor.fromNewest();
    await this.setLevels();
  }

  /** The records of `debugModules` that are loaded go to `level`; the other levels stay. */
  async setLevels(): Promise<void> {
    const want = new Set(this.debugModules);
    const config = await this.dk.logConfig();
    const modules = [...config.modules];
    for (const m of await this.dk.modules())
      if (want.has(m.name) && m.index < modules.length) modules[m.index] = this.level;
    await this.dk.setLogConfig(config.level, modules);
  }

  /** The events seen so far, one per line (the end of a failure message). */
  seen(): string {
    if (this.recent.length === 0) return '(no events seen)';
    return this.recent
      .slice(-this.tail)
      .map((e) => `#${e.seq} ${e.kind} ${e.args.map((a) => (a | 0).toString()).join(' ')}`)
      .join('\n');
  }

  async until(
    kind: LogKindName,
    match: (e: RuntimeEvent) => boolean = () => true,
    { withinMs = 2000 }: WaitOpts = {},
  ): Promise<RuntimeEvent> {
    const start = await this.clock.now();
    const wallStart = Date.now();
    const done = async (ok: boolean) =>
      this.onWait?.({
        kind: 'event',
        what: kind,
        clockMs: (await this.clock.now()) - start,
        wallMs: Date.now() - wallStart,
        ok,
      });
    try {
      for (;;) {
        const hit = await this.scan(kind, match);
        if (hit) {
          await done(true);
          return hit;
        }
        if ((await this.clock.now()) - start >= withinMs)
          throw new Error(`events.until ${kind}: not seen within ${withinMs} ms. Last events:\n${this.seen()}`);
        await this.clock.pause(this.pollMs);
      }
    } catch (e) {
      await done(false);
      throw e;
    }
  }

  untilBoot(opts?: WaitOpts) {
    return this.until('BOOT_START', undefined, { withinMs: 90_000, ...opts });
  }
  untilUiFirst({ loaded, ...opts }: WaitOpts & { loaded?: number } = {}) {
    return this.until('UI_FIRST', (e) => loaded === undefined || Number(e.fields.loaded) >= loaded, {
      withinMs: 90_000,
      ...opts,
    });
  }
  untilModLoad(name: string, opts?: WaitOpts) {
    return this.until('MOD_LOAD', (e) => e.fields.name === name, { withinMs: 30_000, ...opts });
  }
  untilModDsp(index: number, opts?: WaitOpts) {
    return this.until('MOD_DSP', (e) => e.fields.index === index, { withinMs: 30_000, ...opts });
  }
  /** The first UI-loop pass of this boot (FW_READY): the boot intro has ended and the stock MidiRpc service answers. */
  untilFwReady(opts?: WaitOpts) {
    return this.until('FW_READY', undefined, { withinMs: 90_000, ...opts });
  }
  untilSafe(opts?: WaitOpts) {
    return this.until('SAFE', undefined, { withinMs: 90_000, ...opts });
  }
  untilSettingsWrite(opts?: WaitOpts) {
    return this.until('SETTINGS_WRITE', undefined, { withinMs: 10_000, ...opts });
  }
  untilInputTaken(control: number, delta?: number, opts?: WaitOpts) {
    return this.until(
      'INPUT_TAKEN',
      (e) => e.args[0] === 2 && e.args[1] === control && (delta === undefined || (e.args[2]! | 0) === delta),
      { withinMs: 2000, ...opts },
    );
  }

  /** Read what is new and return the first matching event; later events stay queued for the next wait. */
  private async scan(kind: LogKindName, match: (e: RuntimeEvent) => boolean): Promise<RuntimeEvent | null> {
    for (;;) {
      while (this.queue.length > 0) {
        const e = this.queue.shift()!;
        if (e.kind === kind && match(e)) return e;
      }
      const r = await this.cursor.read();
      if (r.newBoot) {
        this.recent = [];
        await this.setLevels().catch(() => undefined); // a module not registered yet: call listen() later
      }
      const events = r.events.map(runtimeEvent);
      for (const e of events) {
        this.recent.push(e);
        if (this.recent.length > 4 * this.tail) this.recent.shift();
      }
      if (r.lost > 0) throw new Error(`events: the log lost ${r.lost} events before they were read\n${this.seen()}`);
      if (events.length === 0) return null;
      this.queue = events;
    }
  }
}
