import fs from 'node:fs';
import path from 'node:path';

/** The status fields the watchdog reads (the daemon's `status` reply has more). */
interface WatchedStatus {
  mode?: string;
  job?: unknown;
  dsp_frames?: unknown;
  cf?: { mainloop_hits?: unknown };
  audio?: { frames?: unknown } | null;
}

/** The part of a session the watchdog uses. */
export interface WatchedSession {
  /** The daemon status; only `mode`, `job`, `dsp_frames`, `cf.mainloop_hits` and `audio.frames` are read. */
  status(): Promise<object>;
  close(): Promise<void>;
  /** Closing its socket fails the pending RPCs at once. */
  client?: { close(): void };
  runDir?: string;
}

export interface Watchdog {
  stop(): void;
  /** Use MS as the no-progress limit (a long multi-boot load, with the same bound as its timeout); undefined restores the limit it started with. */
  setLimit(ms: number | undefined): void;
  /** The message written when the watchdog closed the session; undefined before. */
  readonly tripped: string | undefined;
}

export interface WatchdogOptions {
  intervalMs?: number;
  limitMs?: number;
  now?: () => number;
  log?: (line: string) => void;
}

/** Closes SESSION when its emulated progress (DSP frames, main loop hits, audio frames) stays the same for
 * DIGI_NO_PROGRESS_MS (default 120000, 0 turns it off) of wall time, so a stuck emulator fails the pending test RPC at
 * once. A paused emulator without a job is idle on purpose (host work between steps) and resets the timer. */
export function startNoProgressWatchdog(session: WatchedSession, options: WatchdogOptions = {}): Watchdog {
  const intervalMs = options.intervalMs ?? 5000;
  const baseLimitMs = options.limitMs ?? Number(process.env.DIGI_NO_PROGRESS_MS ?? 120_000);
  if (baseLimitMs <= 0) return { stop() {}, setLimit() {}, tripped: undefined };
  let limitMs = baseLimitMs;
  const now = options.now ?? Date.now;
  const log =
    options.log ?? ((line: string) => console.error(`${line} [${session.runDir ?? 'session'}, limit ${limitMs} ms]`));
  let lastKey: string | undefined;
  let lastChange = now();
  let stopped = false;
  let busy = false;
  let tripped: string | undefined;

  const trip = (frames: unknown, mainloop: unknown): void => {
    const seconds = Math.round((now() - lastChange) / 1000);
    tripped = `no emulated progress for ${seconds} s (frames ${frames}, mainloop ${mainloop})`;
    stopped = true;
    clearInterval(timer);
    const line = `watchdog: ${tripped}`;
    log(line);
    try {
      if (session.runDir) fs.appendFileSync(path.join(session.runDir, 'watchdog.log'), `${line}\n`);
    } catch {
      /* The artifact is optional. */
    }
    try {
      session.client?.close();
    } catch {
      /* Already closed. */
    }
    void session.close().catch(() => {});
  };

  const tick = async (): Promise<void> => {
    if (busy || stopped) return;
    busy = true;
    let timeout: ReturnType<typeof setTimeout> | undefined;
    try {
      // A status call that does not answer within one interval counts as no progress.
      const status = await Promise.race([
        session.status() as Promise<WatchedStatus>,
        new Promise<undefined>((resolve) => {
          timeout = setTimeout(() => resolve(undefined), intervalMs);
        }),
      ]).catch(() => undefined);
      if (stopped) return;
      const t = now();
      if (status?.mode === 'paused' && !status.job) {
        lastChange = t;
        return;
      }
      const mainloop = status?.cf?.mainloop_hits;
      const key = status ? `${status.dsp_frames}|${mainloop}|${status.audio?.frames}` : undefined;
      if (status && key !== lastKey) {
        lastKey = key;
        lastChange = t;
      } else if (t - lastChange >= limitMs) {
        trip(status?.dsp_frames, mainloop);
      }
    } finally {
      clearTimeout(timeout);
      busy = false;
    }
  };

  const timer = setInterval(() => void tick().catch(() => {}), intervalMs);
  timer.unref();
  return {
    stop() {
      stopped = true;
      clearInterval(timer);
    },
    setLimit(ms) {
      limitMs = ms !== undefined && ms > 0 ? Math.max(ms, baseLimitMs) : baseLimitMs;
    },
    get tripped() {
      return tripped;
    },
  };
}
