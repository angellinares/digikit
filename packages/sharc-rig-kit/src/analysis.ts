/**
 * The analysis of a rendered signal for the sound checks (tests/rig/*.toml): level, pitch, decay, tones and the comparison with a
 * reference table. Plain functions on arrays of numbers (float32 samples as numbers), tested on synthetic signals.
 */
import { createHash } from 'node:crypto';

export const FS = 48000;

export const peak = (x: readonly number[]): number => x.reduce((m, v) => Math.max(m, Math.abs(v)), 0);

export const mean = (x: readonly number[]): number => x.reduce((s, v) => s + v, 0) / x.length;

export const rms = (x: readonly number[]): number => Math.sqrt(x.reduce((s, v) => s + v * v, 0) / x.length);

/** The largest |a - b| over the shorter length; Infinity when the lengths differ (a missing sample is a difference). */
export function maxAbsDiff(a: readonly number[], b: readonly number[]): number {
  if (a.length !== b.length) return Number.POSITIVE_INFINITY;
  return a.reduce((m, v, i) => Math.max(m, Math.abs(v - b[i]!)), 0);
}

/** The SHA-256 of the samples as little-endian float32 words (the pin of a reference signal). */
export function wordsHash(x: readonly number[]): string {
  return createHash('sha256')
    .update(Buffer.from(Float32Array.from(x).buffer))
    .digest('hex');
}

/** Little-endian float32 file contents to numbers. */
export function readF32(bytes: Uint8Array): number[] {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  return Array.from({ length: Math.floor(bytes.byteLength / 4) }, (_, i) => view.getFloat32(4 * i, true));
}

/** numpy's Hann window. */
export const hanning = (n: number): number[] =>
  n === 1 ? [1] : Array.from({ length: n }, (_, i) => 0.5 - 0.5 * Math.cos((2 * Math.PI * i) / (n - 1)));

/** Fractional sample positions of the upward zero crossings (negative before, non-negative after), by linear interpolation. */
export function upwardCrossings(x: readonly number[]): number[] {
  const t: number[] = [];
  for (let i = 0; i + 1 < x.length; i++) if (x[i]! < 0 && x[i + 1]! >= 0) t.push(i + -x[i]! / (x[i + 1]! - x[i]!));
  return t;
}

/** Slope of the least-squares line through (k, t[k]). */
export function lineSlope(t: readonly number[]): number {
  const n = t.length;
  const mk = (n - 1) / 2;
  const mt = mean(t);
  let num = 0;
  let den = 0;
  t.forEach((v, k) => {
    num += (k - mk) * (v - mt);
    den += (k - mk) ** 2;
  });
  return num / den;
}

/** Frequency of a clean periodic signal from the first and last upward crossing (needs three crossings). */
export function pitchByCrossings(x: readonly number[], fs = FS): number {
  const t = upwardCrossings(x);
  if (t.length < 3) throw new Error('fewer than two periods');
  return (fs * (t.length - 1)) / (t.at(-1)! - t[0]!);
}

/** Discrete Fourier transform of any length (radix 2, or Bluestein's chirp for the others). `inverse` is unscaled. */
export function fft(re: readonly number[], im: readonly number[], inverse = false): [number[], number[]] {
  const n = re.length;
  if (n <= 1) return [[...re], [...im]];
  if ((n & (n - 1)) === 0) return radix2(re, im, inverse);
  const sign = inverse ? 1 : -1;
  let m = 1;
  while (m < 2 * n - 1) m <<= 1;
  // chirp w[k] = exp(sign i pi k^2 / n); k^2 is reduced modulo 2n to keep the angle small
  const wr = Array.from<number>({ length: n });
  const wi = Array.from<number>({ length: n });
  for (let k = 0; k < n; k++) {
    const a = (sign * Math.PI * ((k * k) % (2 * n))) / n;
    wr[k] = Math.cos(a);
    wi[k] = Math.sin(a);
  }
  const ar = Array.from<number>({ length: m }).fill(0);
  const ai = Array.from<number>({ length: m }).fill(0);
  const br = Array.from<number>({ length: m }).fill(0);
  const bi = Array.from<number>({ length: m }).fill(0);
  for (let k = 0; k < n; k++) {
    ar[k] = re[k]! * wr[k]! - im[k]! * wi[k]!;
    ai[k] = re[k]! * wi[k]! + im[k]! * wr[k]!;
  }
  br[0] = wr[0]!;
  bi[0] = -wi[0]!;
  for (let k = 1; k < n; k++) {
    br[k] = br[m - k] = wr[k]!;
    bi[k] = bi[m - k] = -wi[k]!;
  }
  const [fr, fi] = radix2(ar, ai, false);
  const [gr, gi] = radix2(br, bi, false);
  const pr = fr.map((v, i) => v * gr[i]! - fi[i]! * gi[i]!);
  const pi = fr.map((v, i) => v * gi[i]! + fi[i]! * gr[i]!);
  const [cr, ci] = radix2(pr, pi, true);
  const outR = Array.from<number>({ length: n });
  const outI = Array.from<number>({ length: n });
  for (let k = 0; k < n; k++) {
    const xr = cr[k]! / m;
    const xi = ci[k]! / m;
    outR[k] = xr * wr[k]! - xi * wi[k]!;
    outI[k] = xr * wi[k]! + xi * wr[k]!;
  }
  return [outR, outI];
}

function radix2(re: readonly number[], im: readonly number[], inverse: boolean): [number[], number[]] {
  const n = re.length;
  const r = [...re];
  const i = [...im];
  for (let a = 1, b = 0; a < n; a++) {
    let bit = n >> 1;
    for (; b & bit; bit >>= 1) b ^= bit;
    b ^= bit;
    if (a < b) {
      [r[a], r[b]] = [r[b]!, r[a]!];
      [i[a], i[b]] = [i[b]!, i[a]!];
    }
  }
  for (let len = 2; len <= n; len <<= 1) {
    const ang = ((inverse ? 2 : -2) * Math.PI) / len;
    const wlr = Math.cos(ang);
    const wli = Math.sin(ang);
    for (let s = 0; s < n; s += len) {
      let cr = 1;
      let ci = 0;
      for (let k = 0; k < len / 2; k++) {
        const u = s + k;
        const v = u + len / 2;
        const tr = r[v]! * cr - i[v]! * ci;
        const ti = r[v]! * ci + i[v]! * cr;
        r[v] = r[u]! - tr;
        i[v] = i[u]! - ti;
        r[u] = r[u]! + tr;
        i[u] = i[u]! + ti;
        [cr, ci] = [cr * wlr - ci * wli, cr * wli + ci * wlr];
      }
    }
  }
  return [r, i];
}

/**
 * Frequency of the fundamental of a harmonic signal: a band-pass around `guess` (Hann window, FFT, keep 0.7 to 1.3 times the guess), the
 * middle 80 % of the result, then a least-squares fit of the upward crossings against their index (the slope is the period).
 */
export function pitchByBandpass(y: readonly number[], guess: number, fs = FS): number {
  const n = y.length;
  const w = hanning(n);
  const [re, im] = fft(
    y.map((v, i) => v * w[i]!),
    Array.from<number>({ length: n }).fill(0),
  );
  const keep = (k: number) => {
    const f = (Math.min(k, n - k) * fs) / n;
    return f >= 0.7 * guess && f <= 1.3 * guess;
  };
  const [xr] = fft(
    re.map((v, k) => (keep(k) ? v : 0)),
    im.map((v, k) => (keep(k) ? v : 0)),
    true,
  );
  const x = xr.map((v) => v / n).slice(Math.floor(n / 10), n - Math.floor(n / 10));
  const t = upwardCrossings(x);
  if (t.length < 3) throw new Error('fewer than two periods in the band');
  return fs / lineSlope(t);
}

/** Amplitude and phase of the component at `f` Hz: a Hann-windowed single-bin projection of `y`, whose first sample is sample number `first`. */
export function projectTone(y: readonly number[], f: number, first = 0, fs = FS): { amplitude: number; phase: number } {
  const n = y.length;
  const w = hanning(n);
  let re = 0;
  let im = 0;
  let ws = 0;
  for (let i = 0; i < n; i++) {
    const a = (-2 * Math.PI * f * (first + i)) / fs;
    re += w[i]! * y[i]! * Math.cos(a);
    im += w[i]! * y[i]! * Math.sin(a);
    ws += w[i]!;
  }
  return { amplitude: (2 * Math.hypot(re, im)) / ws, phase: Math.atan2(im, re) };
}

/** The amplitude of the component of `y` (after `skip` samples) at exactly `f` Hz. */
export const amplitudeAt = (y: readonly number[], f: number, skip = 0, fs = FS): number =>
  projectTone(y.slice(skip), f, 0, fs).amplitude;

/** RMS of each block of `size` samples. */
export function blockRms(x: readonly number[], size = 32): number[] {
  return Array.from({ length: Math.floor(x.length / size) }, (_, b) => rms(x.slice(b * size, (b + 1) * size)));
}

/** Seconds from the loudest block until the block RMS first stays below `peak * 10^(db/20)` for the rest of the signal; null when it never does. */
export function decayTime(x: readonly number[], db: number, size = 32, fs = FS): number | null {
  const env = blockRms(x, size);
  const top = env.reduce((m, v, i) => (v > env[m]! ? i : m), 0);
  const limit = env[top]! * 10 ** (db / 20);
  let last = -1;
  env.forEach((v, i) => {
    if (i >= top && v >= limit) last = i;
  });
  return last + 1 >= env.length ? null : ((last + 1 - top) * size) / fs;
}

/** The unwrapped difference of two phases in a row of phases (radians): the largest |phase[k] - phase[0]| after unwrapping. */
export function phaseDrift(phases: readonly number[]): number {
  let prev = phases[0]!;
  let offset = 0;
  let worst = 0;
  for (const p of phases) {
    let d = p - prev;
    d -= 2 * Math.PI * Math.round(d / (2 * Math.PI));
    offset += d;
    prev = p;
    worst = Math.max(worst, Math.abs(offset));
  }
  return worst;
}
