/**
 * Fingerprint and classifier of a rig pin (docs/TESTING.md "Classifying a changed pin").
 *
 * The `sha256` of a `same-as` check is the strict gate. Next to it a case stores a small fingerprint of the pinned signal as plain numbers
 * (`tests/rig/fingerprint/<key>.json`: peak, onset sample, RMS per block, 64 log-spaced band levels). When the hash differs, `classify` says what kind
 * of change it is: with the previous output (the local cache `out/rig/<key>/ref.f32`, written at pin time, never committed) it compares sample by
 * sample (time shift, rounding, real change); without it, it compares the fingerprints.
 */
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { FS, blockRms, fft, hanning, peak, readF32, wordsHash } from './analysis.ts';
import { ROOT } from './rig.ts';

export const BLOCK = 32;
export const BANDS = 64;
const BAND_LO = 40; // Hz
const BAND_HI = 22000;
const FFT_SIZE = 65536;
const FLOOR_DB = -200;
/** a difference at or below this energy (dB re the signal) with an unchanged spectrum is rounding */
export const ROUNDING_DB = -100;
const SHIFT_RANGE = 256;

export interface Fingerprint {
  version: 1;
  rate: number;
  samples: number;
  block: number;
  peak: number;
  /** first sample with |x| > 1e-6, or -1 */
  onset: number;
  /** RMS of each block of `block` samples (6 significant digits) */
  rms: number[];
  /** the centre frequencies of the bands, Hz (2 digits) */
  bandHz: number[];
  /** the power of each band in dB re full scale (2 decimals) */
  bandDb: number[];
}

const sig = (v: number, digits = 6): number => Number(v.toPrecision(digits));
const dec = (v: number, d = 2): number => Number(v.toFixed(d));
const db10 = (p: number): number => (p > 1e-20 ? Math.max(FLOOR_DB, 10 * Math.log10(p)) : FLOOR_DB);

export function bandEdges(): number[] {
  const r = (BAND_HI / BAND_LO) ** (1 / BANDS);
  return Array.from({ length: BANDS + 1 }, (_, i) => BAND_LO * r ** i);
}

/** Band powers of the Hann-windowed signal (zero padded to 65536 points), in dB. */
export function bandLevels(x: readonly number[], rate = FS): number[] {
  const n = Math.min(x.length, FFT_SIZE);
  const w = hanning(n);
  const norm = w.reduce((s, v) => s + v * v, 0) || 1;
  const re = Array.from<number>({ length: FFT_SIZE }).fill(0);
  for (let i = 0; i < n; i++) re[i] = x[i]! * w[i]!;
  const [r, im] = fft(re, Array.from<number>({ length: FFT_SIZE }).fill(0));
  const edges = bandEdges();
  const power = Array.from<number>({ length: BANDS }).fill(0);
  for (let k = 1; k < FFT_SIZE / 2; k++) {
    const f = (k * rate) / FFT_SIZE;
    if (f < edges[0]! || f >= edges[BANDS]!) continue;
    let b = Math.floor(Math.log(f / BAND_LO) / Math.log(edges[1]! / edges[0]!));
    b = Math.min(BANDS - 1, Math.max(0, b));
    power[b] += (2 * (r[k]! ** 2 + im[k]! ** 2)) / (norm * FFT_SIZE);
  }
  return power.map(db10);
}

export function fingerprint(x: readonly number[], rate = FS): Fingerprint {
  const edges = bandEdges();
  return {
    version: 1,
    rate,
    samples: x.length,
    block: BLOCK,
    peak: sig(peak(x)),
    onset: x.findIndex((v) => Math.abs(v) > 1e-6),
    rms: blockRms(x, BLOCK).map((v) => sig(v)),
    bandHz: edges.slice(0, BANDS).map((lo, i) => dec(Math.sqrt(lo * edges[i + 1]!), 1)),
    bandDb: bandLevels(x, rate).map((v) => dec(v)),
  };
}

// ---- where the files are --------------------------------------------------------------------------------------------------------

/** A short stable name of a case: `<module>/<device>/<case name>` as one directory name. */
export function rigKey(module: string, device: string, caseName: string): string {
  const slug = (s: string) =>
    s
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, '-')
      .replace(/^-|-$/g, '');
  return `${slug(module)}.${device}.${slug(caseName)}`;
}

export const fingerprintFile = (testsRigDir: string, key: string): string =>
  path.join(testsRigDir, 'fingerprint', `${key}.json`);
/** `out/rig/<key>/`: the local cache of the pinned output (`ref.f32`) and the files of the report. `out/` is shared by the worktrees: a key names module, device and case. */
export const cacheDir = (key: string): string => path.join(ROOT, 'out/rig', key);

export function writeFingerprint(file: string, fp: Fingerprint): void {
  mkdirSync(path.dirname(file), { recursive: true });
  writeFileSync(file, `${JSON.stringify(fp)}\n`);
}

export function readFingerprint(file: string): Fingerprint | null {
  return existsSync(file) ? (JSON.parse(readFileSync(file, 'utf8')) as Fingerprint) : null;
}

export function writeRef(key: string, x: readonly number[]): void {
  mkdirSync(cacheDir(key), { recursive: true });
  writeFileSync(path.join(cacheDir(key), 'ref.f32'), Buffer.from(Float32Array.from(x).buffer));
}

export function readRef(key: string): number[] | null {
  const file = path.join(cacheDir(key), 'ref.f32');
  return existsSync(file) ? readF32(readFileSync(file)) : null;
}

// ---- the classifier -------------------------------------------------------------------------------------------------------------

export type Kind = 'identical' | 'time-shift' | 'rounding' | 'real-change' | 'fingerprint-only';

export interface Verdict {
  kind: Kind;
  /** one line: the kind and the numbers */
  line: string;
  shift?: number;
  diffDb?: number;
  levelDb?: number;
  maxBandDb?: number;
}

const sumSq = (x: readonly number[]): number => x.reduce((s, v) => s + v * v, 0);

/** Energy of `a - b` relative to `b`, in dB (-Infinity for equal signals). */
export function diffDb(a: readonly number[], b: readonly number[]): number {
  let d = 0;
  for (let i = 0; i < Math.min(a.length, b.length); i++) d += (a[i]! - b[i]!) ** 2;
  return d === 0 ? -Infinity : 10 * Math.log10(d / Math.max(sumSq(b), 1e-30));
}

function bestShift(a: readonly number[], r: readonly number[]): number {
  let best = 0;
  let bestC = -Infinity;
  for (let k = -SHIFT_RANGE; k <= SHIFT_RANGE; k++) {
    let c = 0;
    for (let i = Math.max(0, -k); i < Math.min(r.length, a.length - k); i++) c += r[i]! * a[i + k]!;
    if (c > bestC) {
      bestC = c;
      best = k;
    }
  }
  return best;
}

/** The overlap of `a` shifted by `k` against `r` (`a[i + k]` against `r[i]`). */
function overlap(a: readonly number[], r: readonly number[], k: number): [number[], number[]] {
  const lo = Math.max(0, -k);
  const hi = Math.min(r.length, a.length - k);
  return [a.slice(lo + k, hi + k), r.slice(lo, hi)];
}

const fmt = (v: number, d = 2): string => (Number.isFinite(v) ? v.toFixed(d) : v > 0 ? 'inf' : '-inf');

/** Ranges (as `[first, last]`) of indices where `moved(i)` holds, at most `max` of them. */
function ranges(n: number, moved: (i: number) => boolean, max = 3): [number, number][] {
  const out: [number, number][] = [];
  for (let i = 0; i < n; i++)
    if (moved(i)) {
      const last = out.at(-1);
      if (last && last[1] === i - 1) last[1] = i;
      else out.push([i, i]);
    }
  return out.slice(0, max);
}

function describeMoves(
  fa: Fingerprint,
  fb: Fingerprint,
): { blocks: string; bands: string; maxBand: number; level: number } {
  const n = Math.min(fa.rms.length, fb.rms.length);
  const db = (p: number, q: number) => 20 * Math.log10(Math.max(p, 1e-9) / Math.max(q, 1e-9));
  const blocks = ranges(n, (i) => Math.abs(db(fa.rms[i]!, fb.rms[i]!)) > 1 && Math.max(fa.rms[i]!, fb.rms[i]!) > 1e-5);
  const delta = fa.bandDb.map((v, i) => v - fb.bandDb[i]!);
  // bands more than 60 dB under the strongest band do not count: they sit near the leakage floor of the window
  const top = Math.max(...fa.bandDb, ...fb.bandDb) - 60;
  const counts = (i: number) => Math.max(fa.bandDb[i]!, fb.bandDb[i]!) > top;
  const bands = ranges(BANDS, (i) => Math.abs(delta[i]!) > 1 && counts(i));
  const hz = (r: [number, number]) => {
    const mean = delta.slice(r[0], r[1] + 1).reduce((s, v) => s + v, 0) / (r[1] - r[0] + 1);
    return `${Math.round(fa.bandHz[r[0]]!)}-${Math.round(fa.bandHz[r[1]]!)} Hz ${mean > 0 ? '+' : ''}${fmt(mean, 1)} dB`;
  };
  const total = (f: Fingerprint) =>
    10 *
    Math.log10(
      Math.max(
        f.rms.reduce((s, v) => s + v * v, 0),
        1e-30,
      ),
    );
  return {
    blocks: blocks.length ? blocks.map(([a, b]) => `${a}-${b}`).join(',') : 'none',
    bands: bands.length ? bands.map(hz).join('; ') : 'none',
    maxBand: Math.max(0, ...delta.map((v, i) => (counts(i) ? Math.abs(v) : 0))),
    level: total(fa) - total(fb),
  };
}

/**
 * What kind of change is `now` against the pinned output? `ref` is the previous output (the cache) or null; `pinned` the stored fingerprint.
 * With `ref` the verdict rests on the samples; without it, on the fingerprints only (`fingerprint-only`).
 */
export function classify(now: readonly number[], ref: readonly number[] | null, pinned: Fingerprint | null): Verdict {
  const fnow = fingerprint(now);
  if (ref === null) {
    if (pinned === null)
      return { kind: 'fingerprint-only', line: 'no stored fingerprint and no cached output: nothing to compare with' };
    const m = describeMoves(fnow, pinned);
    const same = m.maxBand <= 0.05 && m.blocks === 'none' && fnow.onset === pinned.onset;
    return {
      kind: 'fingerprint-only',
      maxBandDb: m.maxBand,
      levelDb: m.level,
      line: same
        ? `fingerprint-only: envelope, bands and onset equal the pin (band delta <= ${fmt(m.maxBand)} dB), so the change is below its resolution (rounding or phase); no cached output for the sample comparison`
        : `fingerprint-only: onset ${pinned.onset} -> ${fnow.onset}, level ${fmt(m.level)} dB, moved blocks ${m.blocks}, moved bands ${m.bands}; no cached output for the sample comparison`,
    };
  }
  if (ref.length === now.length && ref.every((v, i) => Object.is(v, now[i])))
    return { kind: 'identical', line: 'identical: the output equals the cached output bit for bit' };
  const k = bestShift([...now], ref);
  if (k !== 0) {
    const [a, b] = overlap(now, ref, k);
    const d = diffDb(a, b);
    if (d === -Infinity)
      return {
        kind: 'time-shift',
        shift: k,
        diffDb: d,
        line: `time-shift: the output is the cached output shifted by ${k} samples, bit-identical on the overlap (${a.length} samples)`,
      };
    if (d <= ROUNDING_DB)
      return {
        kind: 'time-shift',
        shift: k,
        diffDb: d,
        line: `time-shift: shifted by ${k} samples, then not bit-identical: difference ${fmt(d, 1)} dB on the overlap`,
      };
  }
  const d = diffDb(now, ref);
  const fref = fingerprint(ref);
  const m = describeMoves(fnow, fref);
  if (d <= ROUNDING_DB && m.maxBand <= 0.05)
    return {
      kind: 'rounding',
      diffDb: d,
      maxBandDb: m.maxBand,
      line: `rounding: difference ${fmt(d, 1)} dB re the signal (limit ${ROUNDING_DB} dB), band spectrum unchanged (max ${fmt(m.maxBand)} dB)`,
    };
  return {
    kind: 'real-change',
    diffDb: d,
    levelDb: m.level,
    maxBandDb: m.maxBand,
    shift: k,
    line: `real change: difference ${fmt(d, 1)} dB re the signal, level ${m.level > 0 ? '+' : ''}${fmt(m.level, 1)} dB, onset ${fref.onset} -> ${fnow.onset}, moved blocks ${m.blocks}, moved bands ${m.bands}`,
  };
}

export { wordsHash };
