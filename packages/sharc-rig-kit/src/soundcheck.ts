/**
 * Sound checks of a module: `modules/<kind>/<name>/tests/rig/*.toml` (docs/TESTING.md "Sound checks").
 *
 * A file holds `[[case]]` tables. A machine case plays notes through core/dsp-voice (`[[case.step]]`: note, velocity, parameter words by
 * manifest id, blocks, trigger, release) and an effect case runs an input through core/fx (`[case.input]`, `[[case.step]]` with the effect's
 * float parameters). Every block is held to the implicit rules (the call returns, the context registers and the guard are fine, the module
 * wrote only its own memory, the output is finite); the `[[case.check]]` tables add the sound: level, pitch, decay, tones, the comparison
 * with a reference table. A check names its `kind` and may limit itself to `blocks = [first, end)` of the case.
 */
import { readFileSync } from 'node:fs';
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
import { REC, RECORDS, f2w, nw } from './context.ts';
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
  /** DT2: write the voice record's word +0x1b8 (ACTIVE and the mark) before the first block: 0 makes the voice idle */
  record_word?: number;
}

export interface RigCase {
  name: string;
  device?: string[];
  context?: 'friendly' | 'hostile';
  track?: number;
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

async function runMachine(module: string, device: Device, c: RigCase, o: RunOptions): Promise<Trace> {
  const spec = machineSpec(module, device);
  if (!spec) throw new Error(`${module} is no machine for ${device}`);
  const rig = await MachineRig.open(spec, { recordStores: true, track: c.track ?? 0, open: o.open });
  const trace: Trace = {
    kind: 'machine',
    out: [],
    input: [],
    master: [],
    instructions: [],
    active: [],
    failure: null,
    stepOf: [],
  };
  try {
    const steps: Step[] = c.step ?? [{}];
    let first = true;
    for (const [si, step] of steps.entries()) {
      if (step.params) rig.setParams(words(step.params));
      if (step.note !== undefined || step.vel !== undefined || first) rig.setNote(step.note ?? 60, step.vel ?? 100);
      first = false;
      if (step.record_word !== undefined) rig.poke(nw(RECORDS + rig.voice * REC + 0x1b8), step.record_word);
      if (step.release) rig.release();
      const trigger = step.trigger ?? !step.release;
      for (let k = 0; k < (step.blocks ?? 1); k++) {
        if (k === 0 && trigger) rig.trigger();
        const b = await rig.block(c.context === 'friendly' ? 'friendly' : undefined);
        trace.out.push(b.samples);
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
  const rng = new PyRandom(Number(c.seed ?? 0xd1617));
  let faults = 0;
  try {
    let k = 0;
    for (const [si, step] of steps.entries()) {
      ((step.params_f32 as number[] | undefined) ?? []).forEach((v, j) => rig.setRowWord(0, P0 + j, f2w(v)));
      for (let n = 0; n < (step.blocks ?? 1); n++, k++) {
        const x = inputBlock(input, k, table, rng);
        const b = await rig.block(x, 'friendly');
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

function inRange(what: string, value: number, c: Toml): string | null {
  if (hasNum(c, 'min') && value < num(c, 'min')) return `${what} ${value} is below ${num(c, 'min')}`;
  if (hasNum(c, 'max') && value > num(c, 'max')) return `${what} ${value} is above ${num(c, 'max')}`;
  return null;
}

/** The signal a check looks at: the output blocks of its range, one channel of an effect (left unless `channel`). */
function signal(t: Trace, c: Check): number[] {
  if (t.kind === 'machine') return slice(t.out, c.blocks);
  const ch = String(c.channel ?? 'left');
  const part = (b: number[]) => (ch === 'right' ? b.slice(32) : ch === 'both' ? b : b.slice(0, 32));
  return slice(t.out.map(part), c.blocks);
}

/** The first message of a check that fails, or null. */
export function evaluate(t: Trace, c: Check, dir: string): string | null {
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
      return h === String(c.sha256) ? null : `the words hash is ${h}, the pin is ${String(c.sha256)}`;
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
    case 'sample-at': {
      const v = t.out[num(c, 'block')]?.[num(c, 'index', 0)];
      if (v === undefined) return `block ${num(c, 'block')} has no sample ${num(c, 'index', 0)}`;
      return Math.abs(v) <= num(c, 'max')
        ? null
        : `sample ${num(c, 'index', 0)} of block ${num(c, 'block')} is ${v}, limit ${num(c, 'max')}`;
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

/** Run every case of a file for the devices it names (both when it names none) and check it. */
export async function runRigFile(module: string, file: string, o: RunOptions = {}): Promise<CheckResult[]> {
  const dir = path.dirname(file);
  const results: CheckResult[] = [];
  const isFx = fxSpec(module) !== null && !DEVICES.some((d) => machineSpec(module, d));
  for (const c of loadRigFile(file).case) {
    const devices = (c.device ?? [...DEVICES]).filter((d): d is Device => (DEVICES as readonly string[]).includes(d));
    for (const device of isFx ? ['fx' as const] : devices) {
      const t = isFx ? await runFx(module, c, dir, o) : await runMachine(module, device as Device, c, o);
      const at = { case: c.name, device };
      results.push({ ...at, kind: 'implicit rules', ok: t.failure === null, message: t.failure ?? '' });
      for (const check of c.check ?? []) {
        let message: string | null;
        try {
          message = evaluate(t, check, dir);
        } catch (e) {
          message = (e as Error).message;
        }
        results.push({ ...at, kind: check.kind, ok: message === null, message: message ?? '' });
      }
    }
  }
  return results;
}
