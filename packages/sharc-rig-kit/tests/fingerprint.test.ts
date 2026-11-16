import { describe, expect, it } from 'vitest';
import { BANDS, classify, fingerprint } from '../src/fingerprint.ts';
import { Raster, plotPng, wavBytes } from '../src/plot.ts';

const N = 6000;
/** a decaying mix of three tones with a little deterministic noise, as float32 words */
const signal = (): number[] => {
  let seed = 12345;
  const noise = () => ((seed = (Math.imul(seed, 1103515245) + 12345) >>> 0) / 2 ** 32 - 0.5) * 2e-3;
  return Array.from({ length: N }, (_, i) => {
    const t = i / 48000;
    const env = Math.exp(-t * 12);
    return Math.fround(
      env *
        (0.5 * Math.sin(2 * Math.PI * 220 * t) +
          0.2 * Math.sin(2 * Math.PI * 3000 * t) +
          0.1 * Math.sin(2 * Math.PI * 9000 * t)) +
        noise(),
    );
  });
};

describe('classify', () => {
  const ref = signal();
  const pinned = fingerprint(ref);

  it('says identical for equal outputs', () => {
    expect(classify(ref, ref, pinned).kind).toBe('identical');
  });

  it('finds a time shift and whether the shifted outputs are bit-identical', () => {
    const now = [...Array.from({ length: 32 }, () => 0), ...ref.slice(0, N - 32)];
    const v = classify(now, ref, pinned);
    expect([v.kind, v.shift, v.diffDb]).toEqual(['time-shift', 32, -Infinity]);
    expect(v.line).toContain('bit-identical');
  });

  it('calls a difference at -120 dB with unchanged bands rounding', () => {
    const rms = Math.sqrt(ref.reduce((s, v) => s + v * v, 0) / N);
    let seed = 7;
    const now = ref.map(
      (v) =>
        v + ((seed = (Math.imul(seed, 1664525) + 1013904223) >>> 0) / 2 ** 32 - 0.5) * 2 * Math.sqrt(3) * rms * 1e-6,
    );
    const v = classify(now, ref, pinned);
    expect(v.kind).toBe('rounding');
    expect(v.diffDb).toBeLessThan(-110);
    expect(v.diffDb).toBeGreaterThan(-130);
  });

  it('reports a level change of -3 dB as a real change with its size', () => {
    const v = classify(
      ref.map((x) => x * 10 ** (-3 / 20)),
      ref,
      pinned,
    );
    expect(v.kind).toBe('real-change');
    expect(v.levelDb).toBeCloseTo(-3, 1);
  });

  it('reports a filter change with the bands that moved', () => {
    let y = 0;
    const now = ref.map((x) => (y += 0.2 * (x - y))); // one-pole low pass: the 3 kHz and 9 kHz tones fall
    const v = classify(now, ref, pinned);
    expect(v.kind).toBe('real-change');
    expect(v.maxBandDb!).toBeGreaterThan(3);
    expect(v.line).toMatch(/moved bands \d+-\d+ Hz -/);
  });

  it('compares fingerprints when there is no cached output', () => {
    expect(classify(ref, null, pinned).line).toContain('equal the pin');
    const v = classify(
      ref.map((x) => x * 0.5),
      null,
      pinned,
    );
    expect([v.kind, v.line]).toEqual(['fingerprint-only', expect.stringContaining('level -6.02 dB')]);
  });
});

describe('fingerprint', () => {
  it('is stable: the same output gives the same numbers, also after a JSON round trip', () => {
    const x = signal();
    const a = fingerprint(x);
    expect(fingerprint(x)).toEqual(a);
    expect(JSON.parse(JSON.stringify(a))).toEqual(a);
    expect([a.rms.length, a.bandDb.length, a.bandHz.length, a.samples]).toEqual([Math.floor(N / 32), BANDS, BANDS, N]);
    expect(a.onset).toBe(0);
  });

  it('puts the energy of a tone in its band', () => {
    const tone = Array.from({ length: 4800 }, (_, i) => Math.fround(0.5 * Math.sin((2 * Math.PI * 1000 * i) / 48000)));
    const f = fingerprint(tone);
    const top = f.bandDb.indexOf(Math.max(...f.bandDb));
    expect(Math.abs(f.bandHz[top]! / 1000 - 1)).toBeLessThan(0.06);
  });
});

describe('files', () => {
  it('writes a PNG of the plot and a float WAV', () => {
    const png = plotPng('t', 'v', signal(), signal());
    expect(png.subarray(1, 4).toString()).toBe('PNG');
    expect(png.readUInt32BE(16)).toBe(1400);
    expect(new Raster(2, 2).png().length).toBeGreaterThan(30);
    const wav = wavBytes([0, 1]);
    expect([wav.subarray(0, 4).toString(), wav.readUInt16LE(20), wav.length]).toEqual(['RIFF', 3, 52]);
  });
});
