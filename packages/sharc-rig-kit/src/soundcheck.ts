/**
 * Sound checks of a module: `modules/<kind>/<name>/tests/rig/*.toml` (docs/TESTING.md "Sound checks").
 *
 * A file holds `[[case]]` tables. A machine case plays notes through core/dsp-voice (`[[case.step]]`: note, velocity, parameter words by
 * manifest id, blocks, trigger, release) and an effect case runs an input through core/fx (`[case.input]`, `[[case.step]]` with the effect's
 * float parameters). Every block is held to the implicit rules (the call returns, the context registers and the guard are fine, the module
 * wrote only its own memory, the output is finite); the `[[case.check]]` tables add the sound: level, pitch, decay, tones, the comparison
 * with a reference table. A check names its `kind` and may limit itself to `blocks = [first, end)` of the case.
 */
import { readFileSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { parse } from 'smol-toml';
import {
  FS,
  amplitudeAt,
  decayTime,
  maxAbsDiff,
  mean,
  peak,
  phaseDrift,
  pitchByBandpass,
  pitchByCrossings,
  projectTone,
  readF32,
  rms,
  wordsHash,
} from './analysis.ts';
import { signalBlock } from './checks.ts';
import {
  classify,
  fingerprint,
  fingerprintFile,
  readFingerprint,
  readRef,
  rigKey,
  writeFingerprint,
  writeRef,
  cacheDir,
} from './fingerprint.ts';
import { plotPng, writePng, writeWav } from './plot.ts';
import { REC, RECORDS, f2w, nw } from './context.ts';
import { PLANE_L, PLANE_R, TOKEN, publish, writePlanes } from './samplepool.ts';
import { P0, FxRig } from './fx.ts';
import { MachineRig } from './machine.ts';
import type { RigOpener } from './ops.ts';
import { PyRandom } from './prng.ts';
import { DEVICES, fxSpec, machineSpec, type Device } from './spec.ts';

export type Toml = Record<string, unknown>;

export interface Check extends Toml {
  kind: string;
  /** `[first, end)` blocks of the case */
  blocks?: [number, number];
}

export interface Step extends Toml {
  note?: number;
  vel?: number;
  params?: Record<string, number>;
  blocks?: number;
  trigger?: boolean;
  release?: boolean;
  /** poke DM words before the first block: `[target, word offset, value]`, the target `rec` or `ctl` (the record or the control words of the first pool entry) */
  pokes?: [string, number, number][];
  /** DT2: write the voice record's word +0x1b8 (ACTIVE and the mark) before the first block: 0 makes the voice idle */
  record_word?: number;
}

/** A sample-pool entry published before the first block (a machine that plays a sample: the planes and the record, `ready` last). */
export interface Pool extends Toml {
  index: number;
  frames: number;
  rate: number;
  channels: number;
  token?: number;
  /** a file of int16 words: the left plane, then the right plane (none for mono), `frames` each */
  planes: string;
  /** byte addresses of the planes (the defaults of samplepool.ts) */
  plane_l?: number;
  plane_r?: number;
}

export interface RigCase {
  name: string;
  device?: string[];
  context?: 'friendly' | 'hostile';
  /** a machine case starts with poisoned (NaN) bss and voice state; `false` zeroes them and needs `poison_reason` */
  poison?: boolean;
  poison_reason?: string;
  track?: number;
  /** DT2: also render the odd voice of the pair after each block (a stereo machine's right channel) */
  right?: boolean;
  pool?: Pool[];
  /** the data symbol of the pool's control sets: after every block the count word of the first entry must be 0 */
  pool_ctl?: string;
  step?: Step[];
  input?: Toml;
  check: Check[];
  [key: string]: unknown;
}

export interface RigFile {
  case: RigCase[];
}

/** What a case produced, block by block. */
export interface Trace {
  kind: 'machine' | 'fx';
  /** per block: the observed output (machine: 32 samples; effect: 64, left then right, of `observe`) */
  out: number[][];
  /** per block, an effect's input (64) */
  input: number[][];
  /** per block, an effect's master after the hook (64) */
  master: number[][];
  instructions: number[];
  /** per block, a machine's voice record word (ACTIVE and mark) */
  active: number[];
  /** the first break of an implicit rule, or null */
  failure: string | null;
  /** per block, which step it belongs to */
  stepOf: number[];
  /** a machine's odd voice (DT2, `right = true`): the right channel, 32 samples a block */
  right?: number[][];
  /** a machine with a sample pool: the six cursor words of the voice's state block after each block */
  cursor?: (number | null)[][];
  /** an effect: the words of its `dk_fx_desc` and the first 1024 words of its state block after the last block */
  desc?: number[];
  state?: (number | null)[];
}

export interface CheckResult {
  case: string;
  device: string;
  kind: string;
  ok: boolean;
  message: string;
}

export function loadRigFile(file: string): RigFile {
  return parse(readFileSync(file, 'utf8')) as unknown as RigFile;
}

const num = (c: Toml, key: string, fallback?: number): number => {
  const v = c[key];
  if (typeof v === 'number') return v;
  if (fallback !== undefined) return fallback;
  throw new Error(`the check needs a number "${key}"`);
};

const hasNum = (c: Toml, key: string): boolean => typeof c[key] === 'number';

/** Samples of the blocks `[first, end)`, as one array. */
function slice(blocks: readonly number[][], range?: [number, number]): number[] {
  return blocks.slice(range?.[0] ?? 0, range?.[1] ?? blocks.length).flat();
}

// ---- running a case -------------------------------------------------------------------------------------------------------------

export interface RunOptions {
  open?: RigOpener;
}

const words = (params: Record<string, number>): [string, number][] => Object.entries(params);

async function runMachine(
  module: string,
  device: Device,
  c: RigCase,
  dir: string,
  o: RunOptions,
  fillSeed?: number,
): Promise<Trace> {
  const spec = machineSpec(module, device);
  if (!spec) throw new Error(`${module} is no machine for ${device}`);
  const rig = await MachineRig.open(spec, {
    recordStores: true,
    track: c.track ?? 0,
    poison: c.poison !== false,
    fillSeed,
    open: o.open,
  });
  const trace: Trace = {
    kind: 'machine',
    out: [],
    right: [],
    cursor: [],
    input: [],
    master: [],
    instructions: [],
    active: [],
    failure: null,
    stepOf: [],
  };
  try {
    const steps: Step[] = c.step ?? [{}];
    const poolAt = (target: string, off: number): number => {
      const entry = c.pool?.[0];
      if (!entry) throw new Error('a poke of rec or ctl needs a pool entry');
      if (target === 'rec') return nw(rig.built.data[spec.sampleRecords!]!) + 8 * entry.index + off;
      if (target === 'ctl') return nw(rig.built.data[c.pool_ctl!]!) + 4 * entry.index + off;
      throw new Error(`unknown poke target ${target}`);
    };
    for (const p of c.pool ?? []) {
      const pcm = readI16(readFileSync(path.join(dir, p.planes)));
      const left = pcm.slice(0, p.frames);
      const right = p.channels === 1 ? left : pcm.slice(p.frames, 2 * p.frames);
      const [pl, pr] = [
        p.plane_l === undefined ? PLANE_L : nw(p.plane_l),
        p.plane_r === undefined ? PLANE_R : nw(p.plane_r),
      ];
      writePlanes((a, w) => rig.poke(a, w), left, right, pl, pr);
      publish(
        (a, w) => rig.poke(a, w),
        nw(rig.built.data[spec.sampleRecords!]!),
        p.index,
        p.frames,
        p.rate,
        p.channels,
        p.token ?? TOKEN,
        pl,
        pr,
      );
    }
    const extra: [number, number][] = [[rig.built.state, 6]];
    if (c.pool_ctl) extra.push([rig.built.data[c.pool_ctl]! + 4 * 4 * (c.pool?.[0]?.index ?? 0) + 4, 1]);
    let first = true;
    for (const [si, step] of steps.entries()) {
      for (const [target, off, value] of step.pokes ?? []) rig.poke(poolAt(target, off), value);
      if (step.params) rig.setParams(words(step.params));
      if (step.note !== undefined || step.vel !== undefined || first) rig.setNote(step.note ?? 60, step.vel ?? 100);
      first = false;
      if (step.record_word !== undefined) rig.poke(nw(RECORDS + rig.voice * REC + 0x1b8), step.record_word);
      if (step.release) rig.release();
      const trigger = step.trigger ?? !step.release;
      for (let k = 0; k < (step.blocks ?? 1); k++) {
        if (k === 0 && trigger) rig.trigger();
        const b = await rig.block(c.context === 'friendly' ? 'friendly' : undefined, extra);
        trace.out.push(b.samples);
        trace.cursor!.push(b.extra[0] ?? []);
        if (c.right) trace.right!.push(await rig.oddBlock());
        if (c.pool_ctl && (b.extra[1]?.[0] ?? 0) !== 0 && trace.failure === null)
          trace.failure = `${c.name} block ${trace.out.length - 1}: the pool's count word is ${b.extra[1]![0]}, not 0`;
        trace.instructions.push(b.instructions);
        trace.active.push(b.active);
        trace.stepOf.push(si);
        trace.failure ??= implicitMachine(c.name, trace.out.length - 1, b, rig.stepLimit);
      }
    }
  } finally {
    await rig.close();
  }
  return trace;
}

function implicitMachine(
  name: string,
  k: number,
  b: Awaited<ReturnType<MachineRig['block']>>,
  limit: number,
): string | null {
  const at = `${name} block ${k}`;
  if (!b.returned) return `${at}: the call did not return within ${limit} steps (halt ${b.halt})`;
  if (b.faults) return `${at}: the runtime guard stepped in (${b.faults} fault(s))`;
  if (b.foreign.length) return `${at}: the module wrote memory it does not own (${b.foreign.join('; ')})`;
  if (b.contextBad.length) return `${at}: the context registers ${b.contextBad.join(',')} did not come back`;
  const bad = b.samples.findIndex((x) => !Number.isFinite(x));
  return bad < 0 ? null : `${at}: output sample ${bad} is not finite`;
}

/** The input of block k of an effect: 64 floats, left then right. */
function inputBlock(input: Toml, k: number, table: number[] | undefined, rng: PyRandom): number[] {
  const kind = String(input.kind);
  if (kind === 'table') return table!.slice(k * 64, k * 64 + 64);
  if (kind === 'sin-index') {
    // 0.25 sin(k + i) for i = 0 .. 63: no two blocks alike, no tone
    return Array.from({ length: 64 }, (_, i) => Math.fround(0.25 * Math.sin(k + i)));
  }
  if (kind === 'sine') {
    const f = num(input, 'freq');
    const amp = num(input, 'amp', 0.5);
    const left = Array.from({ length: 32 }, (_, i) =>
      Math.fround(amp * Math.sin((2 * Math.PI * f * (k * 32 + i)) / FS)),
    );
    return [...left, ...left];
  }
  if (kind === 'square-noise') {
    // the left channel a square of 997 Hz, the right channel random signs, both at `amp`
    const amp = num(input, 'amp', 1);
    const left = Array.from(
      { length: 32 },
      (_, i) => amp * Math.sign(Math.sin((2 * Math.PI * 997 * (k * 32 + i)) / FS)),
    );
    const right = Array.from({ length: 32 }, () => amp * rng.choice([-1, 1]));
    return [...left, ...right].map(Math.fround);
  }
  return signalBlock(kind, k, rng);
}

async function runFx(module: string, c: RigCase, dir: string, o: RunOptions): Promise<Trace> {
  const spec = fxSpec(module);
  if (!spec) throw new Error(`${module} is no effect`);
  const steps: Step[] = c.step ?? [{}];
  const input = (c.input ?? { kind: 'silence' }) as Toml;
  const table = input.kind === 'table' ? readF32(readFileSync(path.join(dir, String(input.file)))) : undefined;
  const mix = typeof c.mix === 'number' ? c.mix : 0x3f800000;
  const first = (steps[0]!.params_f32 as number[] | undefined) ?? [];
  const rig = await FxRig.open(
    [{ pc: spec.process, state: spec.state, desc: spec.desc, params: first.map(f2w), mix }],
    {
      sources: spec.sources,
      recordStores: true,
      open: o.open,
    },
  );
  const trace: Trace = {
    kind: 'fx',
    out: [],
    input: [],
    master: [],
    instructions: [],
    active: [],
    failure: null,
    stepOf: [],
  };
  const observe = c.observe === 'master' ? 'master' : 'scratch';
  const desc = rig.descWords(spec.desc);
  trace.desc = desc;
  // `state_fill`: every word of the declared state starts with this value instead of the rig's poison
  if (typeof c.state_fill === 'number') rig.fillState(0, c.state_fill, Math.floor(desc[3]! / 4));
  const total = steps.reduce((n, st) => n + (st.blocks ?? 1), 0);
  const rng = new PyRandom(Number(c.seed ?? 0xd1617));
  let faults = 0;
  try {
    let k = 0;
    for (const [si, step] of steps.entries()) {
      ((step.params_f32 as number[] | undefined) ?? []).forEach((v, j) => rig.setRowWord(0, P0 + j, f2w(v)));
      for (let n = 0; n < (step.blocks ?? 1); n++, k++) {
        const x = inputBlock(input, k, table, rng);
        const last = k === total - 1;
        const b = await rig.block(x, 'friendly', {}, last ? [[rig.stateAddress(0), 1024]] : []);
        if (last) trace.state = b.extra[0];
        trace.input.push(x);
        trace.master.push(b.master);
        trace.out.push(observe === 'master' ? b.master : b.scratch);
        trace.instructions.push(b.instructions);
        trace.stepOf.push(si);
        const at = `${c.name} block ${k}`;
        if (trace.failure === null) {
          if (!b.returned) trace.failure = `${at}: the hook did not return (halt ${b.halt})`;
          else if (b.faults[0] !== faults) trace.failure = `${at}: the adapter refused the block`;
          else if (b.ctxBad.length || b.regsBad.length)
            trace.failure = `${at}: registers ${[...b.ctxBad, ...b.regsBad].join(',')} did not come back`;
          else {
            const stray = rig.foreignStores(desc[5]!, b.stores);
            if (stray.length) trace.failure = `${at}: the module wrote memory it does not own (${stray.join('; ')})`;
          }
        }
        faults = b.faults[0]!;
      }
    }
  } finally {
    await rig.close();
  }
  return trace;
}

// ---- the checks -----------------------------------------------------------------------------------------------------------------

/** int16 little-endian words. */
function readI16(bytes: Uint8Array): number[] {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  return Array.from({ length: Math.floor(bytes.byteLength / 2) }, (_, i) => view.getInt16(2 * i, true));
}

function inRange(what: string, value: number, c: Toml): string | null {
  if (hasNum(c, 'min') && value < num(c, 'min')) return `${what} ${value} is below ${num(c, 'min')}`;
  if (hasNum(c, 'max') && value > num(c, 'max')) return `${what} ${value} is above ${num(c, 'max')}`;
  return null;
}

/** The signal a check looks at: the output blocks of its range, one channel of an effect (left unless `channel`). */
function signal(t: Trace, c: Check): number[] {
  if (t.kind === 'machine') return slice(c.channel === 'right' ? (t.right ?? []) : t.out, c.blocks);
  const ch = String(c.channel ?? 'left');
  const part = (b: number[]) => (ch === 'right' ? b.slice(32) : ch === 'both' ? b : b.slice(0, 32));
  return slice(t.out.map(part), c.blocks);
}

/** The first message of a check that fails, or null. */
export function evaluate(t: Trace, c: Check, dir: string, key?: string): string | null {
  const x = (): number[] => signal(t, c);
  switch (c.kind) {
    case 'rms':
      return inRange('RMS', rms(x()), c);
    case 'peak':
      return inRange('peak', peak(x()), c);
    case 'dc':
      return Math.abs(mean(x())) > num(c, 'max') ? `DC offset ${mean(x())} is beyond ${num(c, 'max')}` : null;
    case 'no-clip':
      return peak(x()) > num(c, 'limit', 1) ? `peak ${peak(x())} clips at ${num(c, 'limit', 1)}` : null;
    case 'finite':
      return x().every(Number.isFinite) ? null : 'a sample is not finite';
    case 'silent-after': {
      const after = slice(t.out, [num(c, 'block'), t.out.length]);
      const top = peak(after);
      return top > num(c, 'max', 0) ? `the signal is not silent after block ${num(c, 'block')}: peak ${top}` : null;
    }
    case 'decay-time': {
      const s = decayTime(x(), num(c, 'db'));
      if (s === null) return `the level never falls below ${num(c, 'db')} dB of its peak`;
      return inRange(`decay to ${num(c, 'db')} dB takes (s)`, s, c);
    }
    case 'pitch-hz': {
      const want = hasNum(c, 'expect') ? num(c, 'expect') : 440 * 2 ** ((num(c, 'note') - 69) / 12);
      const skip = hasNum(c, 'skip_periods') ? Math.floor((num(c, 'skip_periods') * FS) / want) : 0;
      const y = x().slice(skip);
      const got = c.method === 'bandpass' ? pitchByBandpass(y, want) : pitchByCrossings(y);
      const cents = 1200 * Math.log2(got / want);
      return Math.abs(cents) < num(c, 'cents', 1)
        ? null
        : `pitch ${got} Hz is ${cents.toFixed(3)} cents from ${want} Hz (limit ${num(c, 'cents', 1)})`;
    }
    case 'matches-table': {
      const want = readF32(readFileSync(path.join(dir, String(c.file))));
      const got = signal(t, c);
      if (got.length !== want.length) return `the table has ${want.length} samples, the run ${got.length}`;
      const d = maxAbsDiff(got, want);
      return d <= num(c, 'atol')
        ? null
        : `the largest difference to ${String(c.file)} is ${d} (limit ${num(c, 'atol')})`;
    }
    case 'same-as': {
      const h = wordsHash(x());
      if (h === String(c.sha256)) return null;
      const mismatch = `the words hash is ${h}, the pin is ${String(c.sha256)}`;
      if (!key) return mismatch;
      return `${mismatch}\n  ${classify(x(), readRef(key), readFingerprint(fingerprintFile(dir, key))).line}`;
    }
    case 'instructions': {
      const b =
        c.of === 'first'
          ? t.instructions.slice(0, 1)
          : c.of === 'idle'
            ? t.instructions.slice(-1)
            : t.instructions.slice(1);
      const worst = Math.max(...b);
      return worst <= num(c, 'max')
        ? null
        : `${worst} instructions in a block (${String(c.of ?? 'steady')}), limit ${num(c, 'max')}`;
    }
    case 'tone': {
      const y = x();
      const skip = num(c, 'skip_blocks', 0) * 32;
      const wanted = amplitudeAt(y, num(c, 'freq'), skip);
      if (Math.abs(wanted - num(c, 'amp')) > num(c, 'tol'))
        return `the tone at ${num(c, 'freq')} Hz has amplitude ${wanted}, wanted ${num(c, 'amp')} (tolerance ${num(c, 'tol')})`;
      if (hasNum(c, 'image_freq')) {
        const image = amplitudeAt(y, num(c, 'image_freq'), skip);
        const db = 20 * Math.log10(wanted / image);
        if (db < num(c, 'image_db_min'))
          return `the image at ${num(c, 'image_freq')} Hz is only ${db.toFixed(1)} dB down (wanted ${num(c, 'image_db_min')})`;
      }
      return null;
    }
    case 'tone-stable': {
      // the tone at `freq` keeps its level and phase over `windows` windows of `window_blocks` blocks spread over the run
      const n = num(c, 'windows', 8);
      const len = num(c, 'window_blocks', 400);
      const y = signal(t, { ...c, blocks: undefined } as Check);
      const total = y.length / 32;
      const amps: number[] = [];
      const phases: number[] = [];
      for (let i = 0; i < n; i++) {
        const b0 = Math.round((i * (total - len)) / (n - 1));
        const p = projectTone(y.slice(b0 * 32, (b0 + len) * 32), num(c, 'freq'), b0 * 32);
        amps.push(p.amplitude);
        phases.push(p.phase);
      }
      const spread = Math.max(...amps) - Math.min(...amps);
      if (spread >= num(c, 'amp_spread'))
        return `the amplitude moves by ${spread} over the run (limit ${num(c, 'amp_spread')})`;
      if (Math.abs(amps[0]! - num(c, 'amp')) >= num(c, 'tol'))
        return `the tone has amplitude ${amps[0]}, wanted ${num(c, 'amp')}`;
      const drift = phaseDrift(phases);
      return drift < num(c, 'phase_drift')
        ? null
        : `the phase drifts by ${drift} rad over the run (limit ${num(c, 'phase_drift')})`;
    }
    case 'cursor': {
      // a word of the voice's state block after `block`: `equals`, within `tol` (the word read as signed with `signed`)
      let v = t.cursor?.[num(c, 'block')]?.[num(c, 'index')];
      if (v === undefined || v === null) return `no cursor word ${num(c, 'index')} after block ${num(c, 'block')}`;
      if (c.signed === true && v >= 2 ** 31) v -= 2 ** 32;
      return Math.abs(v - num(c, 'equals')) <= num(c, 'tol', 0)
        ? null
        : `cursor word ${num(c, 'index')} is ${v}, wanted ${num(c, 'equals')} (tolerance ${num(c, 'tol', 0)})`;
    }
    case 'voice-active': {
      const a = t.active[num(c, 'block')];
      return a !== undefined && (a & 0xff) !== 0 ? null : `the voice is not active after block ${num(c, 'block')}`;
    }
    case 'tone-fit': {
      // the amplitude of a sine of `freq` fitted by least squares (sine and cosine, no window) to the blocks of the range
      const y = x();
      const first = (c.blocks?.[0] ?? 0) * 32;
      const w = (2 * Math.PI * num(c, 'freq')) / FS;
      let [ss, sc, cc, sy, cy] = [0, 0, 0, 0, 0];
      y.forEach((v, i) => {
        const s = Math.sin(w * (first + i));
        const co = Math.cos(w * (first + i));
        ss += s * s;
        sc += s * co;
        cc += co * co;
        sy += s * v;
        cy += co * v;
      });
      const det = ss * cc - sc * sc;
      const amp = Math.hypot((sy * cc - cy * sc) / det, (cy * ss - sy * sc) / det);
      return Math.abs(amp - num(c, 'amp')) <= num(c, 'tol')
        ? null
        : `the fitted amplitude at ${num(c, 'freq')} Hz is ${amp}, wanted ${num(c, 'amp')} (tolerance ${num(c, 'tol')})`;
    }
    case 'sample-at': {
      const v = t.out[num(c, 'block')]?.[num(c, 'index', 0)];
      if (v === undefined) return `block ${num(c, 'block')} has no sample ${num(c, 'index', 0)}`;
      return Math.abs(v) <= num(c, 'max')
        ? null
        : `sample ${num(c, 'index', 0)} of block ${num(c, 'block')} is ${v}, limit ${num(c, 'max')}`;
    }
    case 'description': {
      const want = c.words as number[];
      const got = (t.desc ?? []).slice(0, want.length);
      return JSON.stringify(got) === JSON.stringify(want)
        ? null
        : `the description words are ${JSON.stringify(got)}, wanted ${JSON.stringify(want)}`;
    }
    case 'state-word': {
      const v = t.state?.[num(c, 'index')];
      return v === num(c, 'equals') ? null : `state word ${num(c, 'index')} is ${v}, wanted ${num(c, 'equals')}`;
    }
    case 'state-untouched-beyond': {
      // every state word from the declared size on still holds the poison the rig put there: the effect wrote only its declared state
      const from = Math.floor((t.desc?.[3] ?? 0) / 4);
      const bad = (t.state ?? []).findIndex((w, i) => i >= from && w !== num(c, 'poison', 0x7fc00001) >>> 0);
      return bad < 0 ? null : `state word ${bad} (declared size ${from} words) was written`;
    }
    case 'differs': {
      const a = slice(t.out, c.blocks);
      const other = slice(t.out, c.other as [number, number]);
      return maxAbsDiff(a, other) > 0 ? null : 'the two block ranges are equal';
    }
    case 'master-equals-input': {
      const from = num(c, 'from_block', 0);
      for (let k = from; k < t.master.length; k++)
        if (t.master[k]!.some((v, i) => v !== t.input[k]![i])) return `the master differs from the input in block ${k}`;
      return null;
    }
    case 'master-equals-scratch': {
      const from = num(c, 'from_block', 0);
      for (let k = from; k < t.master.length; k++)
        if (t.master[k]!.some((v, i) => v !== t.out[k]![i]))
          return `the master differs from the effect output in block ${k} (observe = "scratch")`;
      return null;
    }
    case 'voice-ends': {
      // the voice record's ACTIVE byte goes to 0 within `within` blocks, not before block `after`, and the mark is cleared with it
      const at = t.active.findIndex((a) => (a & 0xff) === 0);
      if (at < 0) return 'the voice never ended';
      if (at <= num(c, 'after', 0)) return `the voice ended already in block ${at}`;
      if (at > num(c, 'within')) return `the voice ended only in block ${at} (limit ${num(c, 'within')})`;
      return (t.active[at]! & 0xff00) === 0 ? null : 'the note-on mark is not cleared when the voice ends';
    }
    default:
      throw new Error(`unknown check kind "${c.kind}"`);
  }
}

/** The second fill of a machine case: a seeded integer pattern in all module state, next to the NaN fill of the first run. */
export const FILL_SEED = 0x5eed1234;

/** The first difference between the runs of two fills, or null: the output does not depend on the initial state. */
export function stateDependence(a: Trace, b: Trace): string | null {
  for (const key of ['out', 'right'] as const) {
    const x = a[key] ?? [];
    const y = b[key] ?? [];
    for (let k = 0; k < Math.max(x.length, y.length); k++)
      for (let i = 0; i < Math.max(x[k]?.length ?? 0, y[k]?.length ?? 0); i++)
        if (!Object.is(x[k]?.[i], y[k]?.[i]))
          return `the output depends on the initial state: ${key} block ${k} sample ${i} is ${x[k]?.[i]} with NaN words and ${y[k]?.[i]} with a seeded integer fill`;
  }
  return null;
}

/** Run every case of a file for the devices it names (both when it names none) and check it. */
export async function runRigFile(module: string, file: string, o: RunOptions = {}): Promise<CheckResult[]> {
  const dir = path.dirname(file);
  const results: CheckResult[] = [];
  const isFx = fxSpec(module) !== null && !DEVICES.some((d) => machineSpec(module, d));
  for (const c of loadRigFile(file).case) {
    const devices = (c.device ?? [...DEVICES]).filter((d): d is Device => (DEVICES as readonly string[]).includes(d));
    for (const device of isFx ? ['fx' as const] : devices) {
      const t = isFx ? await runFx(module, c, dir, o) : await runMachine(module, device as Device, c, dir, o);
      const at = { case: c.name, device };
      results.push({ ...at, kind: 'implicit rules', ok: t.failure === null, message: t.failure ?? '' });
      if (!isFx && c.poison !== false) {
        const other = await runMachine(module, device as Device, c, dir, o, FILL_SEED);
        const why = stateDependence(t, other);
        results.push({ ...at, kind: 'initial state', ok: why === null, message: why ?? '' });
      }
      for (const check of c.check ?? []) {
        let message: string | null;
        try {
          message = evaluate(t, check, dir, rigKey(module, String(device), c.name));
        } catch (e) {
          message = (e as Error).message;
        }
        results.push({ ...at, kind: check.kind, ok: message === null, message: message ?? '' });
      }
    }
  }
  return results;
}

/**
 * Write the pins of a file: run every case that has a `same-as` check and put the words hash of the check's signal into its `sha256` line.
 * A case with such a check names one device, so the n-th `sha256 = "..."` line of the file belongs to the n-th `same-as` check in file order.
 * Returns the hashes written, as `case [device] hash`. This is the only writer of a rig reference pin: a changed pin is a re-pin and needs review.
 */
export async function pinRigFile(module: string, file: string, o: RunOptions = {}): Promise<string[]> {
  const dir = path.dirname(file);
  const hashes: (string | null)[] = [];
  const lines: string[] = [];
  const errors: string[] = [];
  const isFx = fxSpec(module) !== null && !DEVICES.some((d) => machineSpec(module, d));
  for (const c of loadRigFile(file).case) {
    const pins = (c.check ?? []).filter((k) => k.kind === 'same-as');
    if (pins.length === 0) continue;
    const devices = (c.device ?? [...DEVICES]).filter((d): d is Device => (DEVICES as readonly string[]).includes(d));
    if (devices.length !== 1) throw new Error(`${c.name}: a case with a same-as check names exactly one device`);
    try {
      const t = isFx ? await runFx(module, c, dir, o) : await runMachine(module, devices[0]!, c, dir, o);
      if (t.failure !== null) throw new Error(t.failure);
      if (!isFx && c.poison !== false) {
        const why = stateDependence(t, await runMachine(module, devices[0]!, c, dir, o, FILL_SEED));
        if (why !== null) throw new Error(why);
      }
      for (const k of pins) {
        const x = signal(t, k);
        const h = wordsHash(x);
        const key = rigKey(module, devices[0]!, c.name);
        writeFingerprint(fingerprintFile(dir, key), fingerprint(x));
        writeRef(key, x);
        hashes.push(h);
        lines.push(`${c.name} [${devices[0]}] ${h}`);
      }
    } catch (e) {
      // the case keeps its pin line; the failure is reported after the others are written
      for (let q = 0; q < pins.length; q++) hashes.push(null);
      errors.push(`${c.name} [${devices[0]}]: ${(e as Error).message}`);
    }
  }
  let n = 0;
  const text = readFileSync(file, 'utf8').replace(/^sha256 = "(.*)"$/gm, (line) => {
    const h = hashes[n++];
    return h === null || h === undefined ? line : `sha256 = "${h}"`;
  });
  if (n !== hashes.length) throw new Error(`${file} has ${n} sha256 lines for ${hashes.length} same-as checks`);
  writeFileSync(file, text);
  if (errors.length) throw new Error(`not pinned:\n  ${errors.join('\n  ')}\n(pinned: ${lines.length})`);
  return lines;
}

/**
 * The report of a file: for every case with a `same-as` check, `out/rig/<key>/out.wav`, `ref.wav` (when the cache holds the pinned output) and `plot.png`,
 * and one verdict line (`classify`). Runs the cases; changes nothing in the repository.
 */
export async function reportRigFile(module: string, file: string, o: RunOptions = {}): Promise<string[]> {
  const dir = path.dirname(file);
  const lines: string[] = [];
  const isFx = fxSpec(module) !== null && !DEVICES.some((d) => machineSpec(module, d));
  for (const c of loadRigFile(file).case) {
    const pins = (c.check ?? []).filter((k) => k.kind === 'same-as');
    if (pins.length === 0) continue;
    const device = (c.device ?? [...DEVICES])[0] as Device;
    const key = rigKey(module, device, c.name);
    try {
      const t = isFx ? await runFx(module, c, dir, o) : await runMachine(module, device, c, dir, o);
      const x = signal(t, pins[0]!);
      const ref = readRef(key);
      const same = wordsHash(x) === String(pins[0]!.sha256);
      const v = classify(x, ref, readFingerprint(fingerprintFile(dir, key)));
      const out = cacheDir(key);
      writeWav(path.join(out, 'out.wav'), x);
      if (ref) writeWav(path.join(out, 'ref.wav'), ref);
      writePng(
        path.join(out, 'plot.png'),
        plotPng(`${module} ${device} ${c.name}`, `${same ? 'PIN MATCHES' : 'PIN DIFFERS'}  ${v.line}`, x, ref),
      );
      lines.push(`${key}: ${same ? 'pin matches' : 'PIN DIFFERS'}; ${v.line}\n  ${path.join(out, 'plot.png')}`);
    } catch (e) {
      lines.push(`${key}: not run: ${(e as Error).message}`);
    }
  }
  return lines;
}
