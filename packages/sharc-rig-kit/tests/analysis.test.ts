import { describe, expect, it } from 'vitest';
import {
  FS,
  amplitudeAt,
  blockRms,
  decayTime,
  fft,
  hanning,
  lineSlope,
  maxAbsDiff,
  mean,
  peak,
  phaseDrift,
  pitchByBandpass,
  pitchByCrossings,
  projectTone,
  readF32,
  rms,
  upwardCrossings,
  wordsHash,
} from '../src/analysis.ts';

const sine = (f: number, n: number, amp = 1, phase = 0, first = 0) =>
  Array.from({ length: n }, (_, i) => amp * Math.sin((2 * Math.PI * f * (first + i)) / FS + phase));

describe('level', () => {
  it('measures peak, mean and RMS', () => {
    const x = sine(1000, 4800, 0.5);
    expect(peak(x)).toBeCloseTo(0.5, 3);
    expect(mean(x)).toBeCloseTo(0, 6);
    expect(rms(x)).toBeCloseTo(0.5 / Math.sqrt(2), 4);
    expect(mean([1, 2, 3])).toBe(2);
  });

  it('compares two signals and calls signals of different length different', () => {
    expect(maxAbsDiff([1, 2, 3], [1, 2.5, 3])).toBe(0.5);
    expect(maxAbsDiff([1, 2], [1, 2, 3])).toBe(Number.POSITIVE_INFINITY);
  });
});

describe('words', () => {
  it('hashes float32 words and reads them back from little-endian bytes', () => {
    const x = [0.5, -1.25, 3];
    const bytes = new Uint8Array(Float32Array.from(x).buffer);
    expect(readF32(bytes)).toEqual(x);
    expect(wordsHash(x)).toHaveLength(64);
    expect(wordsHash(x)).not.toBe(wordsHash([0.5, -1.25, 3.0000002]));
    expect(wordsHash(x)).toBe(wordsHash(readF32(bytes)));
  });
});

describe('Fourier transform', () => {
  it('finds the bin of a sine for a power of two and for another length, and inverts', () => {
    for (const n of [64, 100, 77]) {
      const x = Array.from({ length: n }, (_, i) => Math.cos((2 * Math.PI * 5 * i) / n));
      const [re, im] = fft(x, Array.from<number>({ length: n }).fill(0));
      expect(Math.hypot(re[5]!, im[5]!)).toBeCloseTo(n / 2, 6);
      expect(Math.hypot(re[6]!, im[6]!)).toBeLessThan(1e-9);
      const [back] = fft(re, im, true);
      expect(
        maxAbsDiff(
          back.map((v) => v / n),
          x,
        ),
      ).toBeLessThan(1e-9);
    }
  });
});

describe('pitch', () => {
  it('measures a sine from its zero crossings and asks for three crossings', () => {
    expect(pitchByCrossings(sine(440, 4800))).toBeCloseTo(440, 1);
    expect(() => pitchByCrossings(sine(440, 100))).toThrow('fewer than two periods');
    expect(upwardCrossings([-1, 1, -1, 1])).toEqual([0.5, 2.5]);
  });

  it('fits a line through crossings', () => {
    expect(lineSlope([1, 3, 5, 7])).toBeCloseTo(2, 12);
  });

  it('finds the fundamental of a harmonic signal whose second harmonic is stronger', () => {
    const f = 220;
    const x = sine(f, 20000, 0.3).map((v, i) => v + 0.6 * Math.sin((2 * Math.PI * 2 * f * i) / FS + 1));
    expect(Math.abs(1200 * Math.log2(pitchByBandpass(x, f) / f))).toBeLessThan(0.5);
  });

  it('tells a sharp string from the guess by the cents it is off', () => {
    const x = sine(441.3, 19000);
    const cents = 1200 * Math.log2(pitchByBandpass(x, 440) / 440);
    expect(cents).toBeCloseTo(1200 * Math.log2(441.3 / 440), 1);
  });
});

describe('tones', () => {
  it('projects the amplitude of one frequency and ignores one 100 Hz away', () => {
    const x = sine(1000, 6000, 0.5).map((v, i) => v + 0.2 * Math.sin((2 * Math.PI * 1100 * i) / FS));
    expect(amplitudeAt(x, 1000)).toBeCloseTo(0.5, 3);
    expect(amplitudeAt(x, 1100)).toBeCloseTo(0.2, 3);
    expect(amplitudeAt(x, 1500)).toBeLessThan(1e-3);
  });

  it('reads the phase against the sample number, so windows far apart are comparable', () => {
    const f = 1234.5;
    const a = projectTone(sine(f, 4000, 1, 0.7, 0), f, 0);
    const b = projectTone(sine(f, 4000, 1, 0.7, 100000), f, 100000);
    expect(b.phase).toBeCloseTo(a.phase, 3);
    expect(b.amplitude).toBeCloseTo(a.amplitude, 3);
  });

  it('measures the largest drift of a phase row after unwrapping', () => {
    expect(phaseDrift([3.1, -3.1, 3.12])).toBeCloseTo(0.0832, 3); // the wrap from 3.1 to -3.1 is a step of 0.0832 (not of 6.2)
    expect(phaseDrift([0, 0.001, 0.002])).toBeCloseTo(0.002, 6);
  });

  it('has the window numpy has', () => {
    const w = hanning(5);
    expect(w[0]).toBeCloseTo(0, 12);
    expect(w[2]).toBeCloseTo(1, 12);
  });
});

describe('envelope', () => {
  const tail = (decayPerBlock: number, blocks: number) =>
    Array.from({ length: blocks * 32 }, (_, i) => decayPerBlock ** Math.floor(i / 32) * (i % 2 ? 1 : -1));

  it('takes the RMS of each block', () => {
    expect(blockRms([1, 1, 2, 2], 2)).toEqual([1, 2]);
  });

  it('gives the seconds from the loudest block until the level stays below the limit', () => {
    const x = tail(0.9, 200); // -0.915 dB per block
    const t = decayTime(x, -40)!;
    const blocks = Math.ceil(40 / (-20 * Math.log10(0.9)));
    expect(t).toBeCloseTo((blocks * 32) / FS, 6);
  });

  it('is null when the signal never gets that quiet', () => {
    expect(decayTime(tail(0.999, 20), -60)).toBeNull();
  });

  it('counts from the loudest block, not from the start', () => {
    const x = [...Array<number>(64).fill(0.01), ...tail(0.5, 30)];
    const t = decayTime(x, -20)!;
    expect(t).toBeCloseTo((4 * 32) / FS, 6); // 0.5^4 is below -20 dB after four blocks
  });
});
