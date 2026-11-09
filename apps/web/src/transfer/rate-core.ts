// The speed and the time left of a transfer (pure; apps/desktop/ui/tests/transfer-page-ui.test.ts): the rate is smoothed over the last ~5 s of
// progress, so one slow or fast chunk does not make the number jump. Time without new bytes (BEGIN, END, VERIFY, a stall) counts in the
// time of the window, so it pulls the number down; the window is long enough that the VERIFY of one file does not empty it.

export const RATE_WINDOW_MS = 5000;
/** Below this span between the first and last sample the rate is not known yet (one chunk says nothing). */
const MIN_SPAN_MS = 300;

export interface Rate {
  /** The bytes done so far, at time `now` (ms). A smaller total than the last one starts a new transfer. */
  add(now: number, bytes: number): void;
  /** Bytes per second over the window ending at `now`; undefined while unknown; a transfer that stopped decays toward 0. */
  rate(now: number): number | undefined;
  reset(): void;
}

export function createRate(windowMs: number = RATE_WINDOW_MS): Rate {
  let samples: { t: number; b: number }[] = [];
  return {
    add(now, bytes) {
      const last = samples[samples.length - 1];
      if (last && bytes < last.b) samples = [];
      samples.push({ t: now, b: bytes });
      samples = samples.filter((s) => s.t >= now - windowMs);
    },
    rate(now) {
      const live = samples.filter((s) => s.t >= now - windowMs);
      if (live.length === 0) return samples.length ? 0 : undefined;
      const first = live[0] as { t: number; b: number };
      const last = live[live.length - 1] as { t: number; b: number };
      // a stalled transfer: measure up to now, so the number falls instead of freezing
      const end = now - last.t > MIN_SPAN_MS ? now : last.t;
      if (end - first.t < MIN_SPAN_MS) return undefined;
      return Math.max(0, (last.b - first.b) / ((end - first.t) / 1000));
    },
    reset() {
      samples = [];
    },
  };
}

/** "3.1 MB/s", "640 KB/s" (decimal units, as transfer speeds are usually quoted). */
export function fmtRate(bytesPerSecond: number): string {
  if (bytesPerSecond >= 1e6) return `${(bytesPerSecond / 1e6).toFixed(bytesPerSecond >= 1e7 ? 0 : 1)} MB/s`;
  if (bytesPerSecond >= 1e3) return `${Math.round(bytesPerSecond / 1e3)} KB/s`;
  return `${Math.round(bytesPerSecond)} B/s`;
}

/** Seconds left at `rate`, or undefined when the rate is unknown or zero. */
export const etaSeconds = (remainingBytes: number, rate: number | undefined): number | undefined =>
  rate !== undefined && rate > 0 && remainingBytes >= 0 ? remainingBytes / rate : undefined;

/** "12 s left", "1 min 5 s left", "about 2 h left" (a long estimate is rounded). */
export function fmtEta(seconds: number): string {
  const s = Math.max(1, Math.round(seconds));
  if (s < 60) return `${s} s left`;
  if (s < 3600) return `${Math.floor(s / 60)} min ${s % 60} s left`;
  return `about ${Math.round(s / 3600)} h left`;
}

/** The right-hand text of the progress bar: the percent, then the speed (also `0 B/s` in a stall) and the time left while the transfer runs. */
export function statsLine(percent: number, running: boolean, rate: number | undefined, remainingBytes: number): string {
  const parts = [`${percent}%`];
  if (running && rate !== undefined) {
    parts.push(fmtRate(rate));
    const eta = etaSeconds(remainingBytes, rate);
    if (eta !== undefined && remainingBytes > 0) parts.push(fmtEta(eta));
  }
  return parts.join(' · ');
}
