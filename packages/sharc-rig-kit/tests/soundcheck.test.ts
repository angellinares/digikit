import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { afterAll, describe, expect, it } from 'vitest';
import { FS, wordsHash } from '../src/analysis.ts';
import { evaluate, loadRigFile, type Check, type Trace } from '../src/soundcheck.ts';

const dirs: string[] = [];
afterAll(() => {
  for (const d of dirs) rmSync(d, { recursive: true });
});

/** A machine trace of `blocks` blocks of 32 samples from a function of the sample number. */
function trace(blocks: number, f: (n: number) => number, extra: Partial<Trace> = {}): Trace {
  const out = Array.from({ length: blocks }, (_, b) =>
    Array.from({ length: 32 }, (_, i) => Math.fround(f(b * 32 + i))),
  );
  return {
    kind: 'machine',
    out,
    input: [],
    master: [],
    instructions: Array.from({ length: blocks }, () => 100),
    active: Array.from({ length: blocks }, () => 1),
    failure: null,
    stepOf: Array.from({ length: blocks }, () => 0),
    ...extra,
  };
}

const sine =
  (hz: number, amp = 1) =>
  (n: number) =>
    amp * Math.sin((2 * Math.PI * hz * n) / FS);
const check = (t: Trace, c: Check, dir = '.') => evaluate(t, c, dir);

describe('level checks', () => {
  const t = trace(100, sine(1000, 0.5));

  it('hold the RMS and the peak to their limits and say which limit broke', () => {
    expect(check(t, { kind: 'rms', min: 0.35, max: 0.36 })).toBeNull();
    expect(check(t, { kind: 'rms', max: 0.3 })).toContain('is above 0.3');
    expect(check(t, { kind: 'peak', min: 0.6 })).toContain('is below 0.6');
    expect(check(t, { kind: 'peak', min: 0.49, max: 0.5 })).toBeNull();
  });

  it('look only at the blocks of their range', () => {
    const loud = trace(10, (n) => (n < 160 ? 0.9 : 0.1));
    expect(check(loud, { kind: 'peak', blocks: [0, 5], min: 0.89 })).toBeNull();
    expect(check(loud, { kind: 'peak', blocks: [5, 10], max: 0.1001 })).toBeNull();
    expect(check(loud, { kind: 'peak', blocks: [5, 10], min: 0.89 })).not.toBeNull();
  });

  it('measure DC, clipping and non-finite samples', () => {
    expect(
      check(
        trace(4, () => 0.1),
        { kind: 'dc', max: 0.05 },
      ),
    ).toContain('DC offset');
    expect(check(t, { kind: 'dc', max: 0.01 })).toBeNull();
    expect(
      check(
        trace(4, () => 1.5),
        { kind: 'no-clip' },
      ),
    ).toContain('clips at 1');
    expect(
      check(
        trace(4, () => 1.5),
        { kind: 'no-clip', limit: 2 },
      ),
    ).toBeNull();
    expect(
      check(
        trace(2, (n) => (n === 5 ? Number.NaN : 0)),
        { kind: 'finite' },
      ),
    ).toContain('not finite');
  });

  it('find a signal that is silent from a block on', () => {
    const t2 = trace(10, (n) => (n < 64 ? 0.5 : 0));
    expect(check(t2, { kind: 'silent-after', block: 2 })).toBeNull();
    expect(check(t2, { kind: 'silent-after', block: 1 })).toContain('not silent after block 1');
  });

  it('check one sample of one block', () => {
    expect(
      check(
        trace(3, (n) => (n === 33 ? 0.25 : 0)),
        { kind: 'sample-at', block: 1, index: 1, max: 0.3 },
      ),
    ).toBeNull();
    expect(
      check(
        trace(3, (n) => (n === 33 ? 0.25 : 0)),
        { kind: 'sample-at', block: 1, index: 1, max: 0.2 },
      ),
    ).toContain('is 0.25');
    expect(
      check(
        trace(3, () => 0),
        { kind: 'sample-at', block: 9, max: 1 },
      ),
    ).toContain('has no sample');
  });
});

describe('pitch and decay', () => {
  it('measure the pitch against a note or a frequency, in cents', () => {
    const a = trace(300, sine(440 * 2 ** (3 / 12)));
    expect(check(a, { kind: 'pitch-hz', note: 72, cents: 1 })).toBeNull();
    expect(check(a, { kind: 'pitch-hz', note: 71, cents: 1 })).toContain('cents from');
    expect(check(trace(300, sine(441)), { kind: 'pitch-hz', expect: 440, cents: 1 })).toContain('3.9');
    expect(check(trace(300, sine(441)), { kind: 'pitch-hz', expect: 440, cents: 5, method: 'bandpass' })).toBeNull();
    expect(() => check(trace(1, sine(100)), { kind: 'pitch-hz', expect: 100 })).toThrow('fewer than two periods');
  });

  it('measure the time a level takes to decay and fail when it never does', () => {
    const t = trace(400, (n) => 0.9 ** Math.floor(n / 32) * (n % 2 ? 1 : -1));
    expect(check(t, { kind: 'decay-time', db: -40, min: 0.025, max: 0.035 })).toBeNull();
    expect(check(t, { kind: 'decay-time', db: -40, max: 0.02 })).toContain('is above 0.02');
    expect(
      check(
        trace(10, () => 1),
        { kind: 'decay-time', db: -20 },
      ),
    ).toContain('never falls');
  });

  it('see when a voice ends and that its mark is cleared with it', () => {
    const t = trace(10, () => 0, { active: [1, 1, 1, 0x101, 0, 0, 0, 0, 0, 0] });
    expect(check(t, { kind: 'voice-ends', within: 9 })).toBeNull();
    expect(check(t, { kind: 'voice-ends', within: 3 })).toContain('only in block 4');
    expect(check(t, { kind: 'voice-ends', after: 4, within: 9 })).toContain('already in block 4');
    expect(
      check(
        trace(3, () => 0),
        { kind: 'voice-ends', within: 9 },
      ),
    ).toContain('never ended');
    const marked = trace(3, () => 0, { active: [1, 0x100, 0x100] });
    expect(check(marked, { kind: 'voice-ends', within: 9 })).toContain('mark is not cleared');
  });
});

describe('references', () => {
  const dir = mkdtempSync(path.join(tmpdir(), 'rig-ref-'));
  dirs.push(dir);
  const x = (n: number) => 0.25 * Math.sin(n / 7);
  const t = trace(4, x);
  const samples = t.out.flat();
  writeFileSync(path.join(dir, 'ref.f32'), new Uint8Array(Float32Array.from(samples).buffer));
  writeFileSync(path.join(dir, 'short.f32'), new Uint8Array(Float32Array.from(samples.slice(1)).buffer));

  it('compare the run with a table within a tolerance', () => {
    expect(check(t, { kind: 'matches-table', file: 'ref.f32', atol: 0 }, dir)).toBeNull();
    const off = trace(4, (n) => x(n) + (n === 77 ? 2e-5 : 0));
    expect(check(off, { kind: 'matches-table', file: 'ref.f32', atol: 1e-5 }, dir)).toContain('largest difference');
    expect(check(off, { kind: 'matches-table', file: 'ref.f32', atol: 3e-5 }, dir)).toBeNull();
    expect(check(t, { kind: 'matches-table', file: 'short.f32', atol: 1 }, dir)).toContain('the table has 127 samples');
  });

  it('pin a signal by the hash of its float32 words', () => {
    expect(check(t, { kind: 'same-as', sha256: wordsHash(samples) })).toBeNull();
    expect(check(t, { kind: 'same-as', sha256: 'x'.repeat(64) })).toContain('the words hash is');
  });

  it('refuse a check kind they do not know', () => {
    expect(() => check(t, { kind: 'loudness' })).toThrow('unknown check kind');
  });
});

describe('machine state checks', () => {
  it('tell a voice that is still active from one that ended', () => {
    const t = trace(3, () => 0, { active: [0x101, 1, 0x100] });
    expect(check(t, { kind: 'voice-active', block: 1 })).toBeNull();
    expect(check(t, { kind: 'voice-active', block: 2 })).toContain('not active');
  });

  it('read a word of the cursor and accept a tolerance, with the word read as signed on request', () => {
    const t = trace(2, () => 0, { cursor: [[0, 0, 96, 0, 0xffff0000, 0], []] });
    expect(check(t, { kind: 'cursor', block: 0, index: 2, equals: 96 })).toBeNull();
    expect(check(t, { kind: 'cursor', block: 0, index: 4, equals: -65536, signed: true })).toBeNull();
    expect(check(t, { kind: 'cursor', block: 0, index: 4, equals: -65534, tol: 2, signed: true })).toBeNull();
    expect(check(t, { kind: 'cursor', block: 0, index: 2, equals: 95 })).toContain('is 96');
    expect(check(t, { kind: 'cursor', block: 1, index: 2, equals: 0 })).toContain('no cursor word');
  });

  it('fit the amplitude of a tone over a window shorter than two periods, where a windowed projection is not exact', () => {
    const t = trace(20, sine(150, 0.2));
    expect(check(t, { kind: 'tone-fit', freq: 150, amp: 0.2, tol: 0.001, blocks: [2, 18] })).toBeNull();
    expect(check(t, { kind: 'tone-fit', freq: 150, amp: 0.25, tol: 0.001, blocks: [2, 18] })).toContain(
      'fitted amplitude',
    );
  });
});

describe('instruction limits', () => {
  it('hold the first block, the steady blocks or the last block to a limit', () => {
    const t = trace(4, () => 0, { instructions: [5000, 100, 120, 40] });
    expect(check(t, { kind: 'instructions', of: 'first', max: 5000 })).toBeNull();
    expect(check(t, { kind: 'instructions', of: 'first', max: 4999 })).toContain('5000 instructions');
    expect(check(t, { kind: 'instructions', max: 120 })).toBeNull();
    expect(check(t, { kind: 'instructions', max: 119 })).toContain('120 instructions in a block (steady)');
    expect(check(t, { kind: 'instructions', of: 'idle', max: 40 })).toBeNull();
  });
});

describe('effect checks', () => {
  const fx = (
    master: (k: number) => number[],
    out: (k: number) => number[],
    input: (k: number) => number[],
  ): Trace => ({
    kind: 'fx',
    out: Array.from({ length: 6 }, (_, k) => out(k)),
    master: Array.from({ length: 6 }, (_, k) => master(k)),
    input: Array.from({ length: 6 }, (_, k) => input(k)),
    instructions: [],
    active: [],
    failure: null,
    stepOf: [],
  });
  const block = (v: number) => Array<number>(64).fill(v);

  it('compare the master with the input and with the effect output from a block on', () => {
    const t = fx(
      (k) => block(k < 1 ? 0.5 : 0.25),
      (k) => block(k < 1 ? 0.1 : 0.25),
      () => block(0.5),
    );
    expect(check(t, { kind: 'master-equals-scratch', from_block: 1 })).toBeNull();
    expect(check(t, { kind: 'master-equals-scratch' })).toContain('block 0');
    expect(check(t, { kind: 'master-equals-input' })).toContain('block 1');
    expect(
      check(
        fx(
          () => block(0.5),
          () => block(0),
          () => block(0.5),
        ),
        { kind: 'master-equals-input' },
      ),
    ).toBeNull();
  });

  it('take one channel of the output', () => {
    const t = fx(
      () => block(0),
      () => [...Array<number>(32).fill(0.1), ...Array<number>(32).fill(0.8)],
      () => block(0),
    );
    expect(check(t, { kind: 'peak', max: 0.11 })).toBeNull();
    expect(check(t, { kind: 'peak', channel: 'right', min: 0.8 })).toBeNull();
    expect(check(t, { kind: 'peak', channel: 'both', min: 0.8 })).toBeNull();
  });

  it('read a tone and its image, and the stability of a tone over windows', () => {
    const left = (k: number) =>
      Array.from({ length: 32 }, (_, i) => 0.5 * Math.sin((2 * Math.PI * 1100 * (k * 32 + i)) / FS));
    const t: Trace = {
      kind: 'fx',
      out: Array.from({ length: 200 }, (_, k) => [...left(k), ...left(k)]),
      master: [],
      input: [],
      instructions: [],
      active: [],
      failure: null,
      stepOf: [],
    };
    expect(
      check(t, { kind: 'tone', freq: 1100, amp: 0.5, tol: 0.005, skip_blocks: 20, image_freq: 900, image_db_min: 40 }),
    ).toBeNull();
    expect(check(t, { kind: 'tone', freq: 1200, amp: 0.5, tol: 0.005 })).toContain('has amplitude');
    expect(check(t, { kind: 'tone', freq: 1100, amp: 0.5, tol: 0.005, image_freq: 1100, image_db_min: 40 })).toContain(
      'dB down',
    );
    const stable = {
      kind: 'tone-stable',
      freq: 1100,
      windows: 4,
      window_blocks: 40,
      amp: 0.5,
      tol: 0.01,
      amp_spread: 1e-3,
      phase_drift: 1e-2,
    };
    expect(check(t, stable)).toBeNull();
    expect(check(t, { ...stable, freq: 1100.5, phase_drift: 1e-4 })).not.toBeNull();
  });
});

describe('rig files', () => {
  it('parse into cases with steps and checks', () => {
    const dir = mkdtempSync(path.join(tmpdir(), 'rig-file-'));
    dirs.push(dir);
    const file = path.join(dir, 'x.toml');
    writeFileSync(
      file,
      `[[case]]
name = "a"
device = ["dt2"]
[[case.step]]
note = 60
params = { cutoff = 0x1000 }
blocks = 3
[[case.check]]
kind = "rms"
min = 0.1
`,
    );
    const f = loadRigFile(file);
    expect(f.case).toHaveLength(1);
    expect(f.case[0]!.step![0]).toMatchObject({ note: 60, params: { cutoff: 4096 }, blocks: 3 });
    expect(f.case[0]!.check[0]).toMatchObject({ kind: 'rms', min: 0.1 });
  });
});
