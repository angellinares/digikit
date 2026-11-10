import fs from 'node:fs';
import path from 'node:path';
import { isLoading } from '@digi/test-support';

/** The status fields the watchdog reads (the daemon's `status` reply has more). */
interface WatchedStatus {
  mode?: string;
  job?: unknown;
  epoch?: unknown;
  dsp_frames?: unknown;
  cf?: { mainloop_hits?: unknown; icount?: unknown };
  audio?: { frames?: unknown } | null;
  boot?: { icount?: unknown } | null;
}

const FIELDS = ['epoch', 'dsp_frames', 'cf.mainloop_hits', 'audio.frames', 'cf.icount', 'boot.icount'];

/** The part of a session the watchdog uses. */
export interface WatchedSession {
  /** The daemon status; only `mode`, `job`, `epoch`, `dsp_frames`, `cf.mainloop_hits`, `cf.icount`, `audio.frames` and `boot.icount` are read. */
  status(): Promise<object>;
  /** The same status over a second connection, used when present: a `firmware.load` on the main connection does not delay it. */
  watchStatus?(): Promise<object>;
  close(): Promise<void>;
  /** Closing its socket fails the pending RPCs at once. */
  client?: { close(): void };
  runDir?: string;
}

export interface Watchdog {
  stop(): void;
  /** The message written when the watchdog closed the session; undefined before. */
  readonly tripped: string | undefined;
}

export interface WatchdogOptions {
  intervalMs?: number;
  limitMs?: number;
  now?: () => number;
  log?: (line: string) => void;
}

/** Closes SESSION when its emulated progress stays the same for DIGI_NO_PROGRESS_MS (default 120000, 0 turns it off) of
 * wall time, so a stuck emulator fails the pending test RPC at once. Progress is any change of the DSP frames, the main
 * loop hits, the audio frames and the epoch, and, only while a load or boot runs (`job` or `boot` in the status), the
 * ColdFire instruction count (`cf.icount`) and the count of a boot inside `firmware.load` (`boot.icount`: a cold boot
 * moves only these). The counts are not monotonic
 * (a soft reset or a snapshot restore lowers them): a change in either direction is progress. A paused emulator without a
 * job or boot is idle on purpose (host work between steps) and resets the timer. */
export function startNoProgressWatchdog(session: WatchedSession, options: WatchdogOptions = {}): Watchdog {
  const intervalMs = options.intervalMs ?? 5000;
  const baseLimitMs = options.limitMs ?? Number(process.env.DIGI_NO_PROGRESS_MS ?? 120_000);
  if (baseLimitMs <= 0) return { stop() {}, tripped: undefined };
  const limitMs = baseLimitMs;
  const now = options.now ?? Date.now;
  const log =
    options.log ?? ((line: string) => console.error(`${line} [${session.runDir ?? 'session'}, limit ${limitMs} ms]`));
  let lastKey: string | undefined;
  let lastChange = now();
  let stopped = false;
  let busy = false;
  let tripped: string | undefined;
  let lastParts: unknown[] | undefined;
  /** The progress fields that changed during the session: the log shows them at stop when DIGI_WATCHDOG_LOG is set. */
  const moved = new Set<string>();

  const trip = (status: WatchedStatus | undefined): void => {
    const { dsp_frames: frames, cf, boot } = status ?? {};
    const mainloop = cf?.mainloop_hits;
    const icount = cf?.icount;
    const seconds = Math.round((now() - lastChange) / 1000);
    tripped = `no emulated progress for ${seconds} s (frames ${frames}, mainloop ${mainloop}, icount ${icount}, boot ${JSON.stringify(boot)}${status ? '' : ', status unanswered'})`;
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
        (session.watchStatus ? session.watchStatus() : session.status()) as Promise<WatchedStatus>,
        new Promise<undefined>((resolve) => {
          timeout = setTimeout(() => resolve(undefined), intervalMs);
        }),
      ]).catch(() => undefined);
      if (stopped) return;
      const t = now();
      // A boot inside `firmware.load` (`boot`) is a job too: a paused daemon that loads is not idle.
      if (status?.mode === 'paused' && !isLoading(status)) {
        lastChange = t;
        return;
      }
      const mainloop = status?.cf?.mainloop_hits;
      // The instruction counts are progress only while a load or boot runs: a firmware that spins after its boot
      // moves them too, and the frame and main loop counters are what show it stuck.
      const loading = isLoading(status);
      const parts = status
        ? [
            status.epoch,
            status.dsp_frames,
            mainloop,
            status.audio?.frames,
            loading ? status.cf?.icount : undefined,
            loading ? status.boot?.icount : undefined,
          ]
        : undefined;
      const key = parts?.join('|');
      if (status && key !== lastKey) {
        if (parts && lastParts) parts.forEach((value, i) => value !== lastParts![i] && moved.add(FIELDS[i]!));
        lastParts = parts;
        lastKey = key;
        lastChange = t;
      } else if (t - lastChange >= limitMs) {
        trip(status);
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
      if (!stopped && process.env.DIGI_WATCHDOG_LOG) log(`watchdog: progress moved: ${[...moved].join(',') || 'none'}`);
      stopped = true;
      clearInterval(timer);
    },
    get tripped() {
      return tripped;
    },
  };
}
