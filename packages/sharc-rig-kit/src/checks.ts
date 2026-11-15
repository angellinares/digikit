/**
 * The rules of the conformance run (the checks of the retired Python workers) as functions of one block:
 *
 *  machine  returned   the call came back to the caller within the rig's step limit
 *           guard      the runtime guard of core/dsp-voice did not step in (DT2: its fault count stays 0; a guarded block is zeroed,
 *                      so finite and bound alone would pass a broken routine)
 *           writes     the module's code stored only to the output block, its own voice state, its own data and its stack
 *           context    the context registers (M5 ... MODE1) are as they went in
 *           finite     every output sample is finite (the output block is NaN before the call)
 *           bound      |sample| <= LEVEL_BOUND
 *           budget     instructions of the block <= the voice-block budget; a note-on block <= the note-on budget
 *  effect   returned, context and registers, guard (core/fx did not refuse the block), writes, bounded master, budget
 *
 * Each function returns the message of the first rule a block breaks, or null. A message names the case, the block, the parameter words,
 * the note and the first bad sample, so a failure can be reproduced from it alone.
 */
import { f2w, w2f } from './context.ts';
import type { FxBlock } from './fx.ts';
import type { MachineBlock } from './machine.ts';
import { hex } from './ops.ts';
import type { PyRandom } from './prng.ts';
import { FX_ADAPTER_MAX, LEVEL_BOUND, type Budget } from './spec.ts';

/** Python's repr of a float, for the cases a message shows (nan, inf, and the shortest text of a number). */
export function pyRepr(x: number): string {
  if (Number.isNaN(x)) return 'nan';
  if (!Number.isFinite(x)) return x < 0 ? '-inf' : 'inf';
  return Number.isInteger(x) ? x.toFixed(1) : String(x);
}

const BOUND = LEVEL_BOUND.toFixed(1);

export const describeMachine = (
  group: string,
  name: string,
  k: number,
  words: readonly (readonly [string, number])[],
  note: number,
  vel: number,
): string => {
  const ws = words.map(([id, w]) => `${id}=${hex(w)}`).join(' ') || '(no parameters)';
  return `case ${group}/${name} block ${k}: note ${note} velocity ${vel} words [${ws}]`;
};

export const describeFx = (
  group: string,
  name: string,
  k: number,
  signal: string,
  words: readonly number[],
): string => {
  const ws = words.map((w, j) => `p${j}=${hex(w)}`).join(' ') || '(no parameters)';
  return `case ${group}/${name} block ${k}: signal ${signal} words [${ws}]`;
};

const firstBad = (xs: readonly number[], bad: (x: number) => boolean): number | undefined => {
  const i = xs.findIndex(bad);
  return i < 0 ? undefined : i;
};

/** The first rule of the machine suite that a block breaks (all but the budget), or null. */
export function machineBlockFailure(
  where: string,
  b: MachineBlock,
  stepLimit: number,
  lastCtx: Record<string, number>,
): string | null {
  if (!b.returned)
    return `${where}: the call did not return to the caller within ${stepLimit} steps (halt ${b.halt} at ${hex(b.finalPc)}, ${b.instructions} instructions)`;
  if (b.faults)
    return `${where}: the runtime guard stepped in (${b.faults} fault(s) counted: the routine's output was not finite or above ${BOUND}, and the guard zeroed the block)`;
  if (b.foreign.length) return `${where}: the module wrote memory it does not own (${b.foreign.join('; ')})`;
  if (b.contextBad.length)
    return `${where}: the context registers ${b.contextBad.join(',')} did not come back as they went in (in ${b.contextBad.map((n) => `${n}=${hex(lastCtx[n]!)}`).join(', ')})`;
  const bad = firstBad(b.samples, (x) => !Number.isFinite(x));
  if (bad !== undefined)
    return `${where}: output sample ${bad} of 32 is ${pyRepr(b.samples[bad]!)} (not finite; the output block is NaN before the call, so this sample may be one the routine did not write)`;
  const big = firstBad(b.samples, (x) => Math.abs(x) > LEVEL_BOUND);
  if (big !== undefined)
    return `${where}: output sample ${big} is ${pyRepr(b.samples[big]!)}, above the level bound ${BOUND}`;
  return null;
}

/** The budget rule: a note-on block may take the note-on budget, every other block the voice-block budget. */
export function machineBudgetFailure(where: string, instructions: number, noteOn: boolean, bud: Budget): string | null {
  if (noteOn)
    return instructions > bud.noteOnBlock
      ? `${where}: ${instructions} instructions in a note-on block, budget ${bud.noteOnBlock}`
      : null;
  return instructions > bud.voiceBlock
    ? `${where}: ${instructions} instructions in a block, budget ${bud.voiceBlock} per voice`
    : null;
}

/** The first rule of the effect suite that a block breaks, or null. `faultsBefore` is the fault count of the effect's row before the call; `foreign` the stores outside the write set. */
export function fxBlockFailure(
  where: string,
  b: FxBlock,
  stepLimit: number,
  faultsBefore: number,
  foreign: () => string[],
): string | null {
  if (!b.returned)
    return `${where}: the hook did not return to the stock function within ${stepLimit} steps (halt ${b.halt} at ${hex(b.finalPc)}, ${b.instructions} instructions)`;
  if (b.faults[0] !== faultsBefore) {
    const bad = firstBad(b.scratch, (x) => !Number.isFinite(x) || Math.abs(x) > LEVEL_BOUND);
    const what =
      bad !== undefined
        ? `output sample ${bad} of 64 is ${pyRepr(b.scratch[bad]!)}`
        : 'the process routine returned a negative number';
    return `${where}: the adapter's guard stepped in (${what}; the block played dry)`;
  }
  if (b.ctxBad.length || b.regsBad.length)
    return `${where}: registers ${[...b.ctxBad, ...b.regsBad].join(',')} did not come back as they went in`;
  const stray = foreign();
  if (stray.length) return `${where}: the module wrote memory it does not own (${stray.join('; ')})`;
  const bad = firstBad(b.master, (x) => !Number.isFinite(x) || Math.abs(x) > LEVEL_BOUND);
  if (bad !== undefined) return `${where}: master sample ${bad} is ${pyRepr(b.master[bad]!)} after the hook`;
  return null;
}

/** The budget rule of an effect: its own budget plus the adapter's. */
export function fxBudgetFailure(where: string, instructions: number, effectBudget: number): string | null {
  return instructions > effectBudget + FX_ADAPTER_MAX
    ? `${where}: ${instructions} instructions in a block, budget ${effectBudget} for the effect and ${FX_ADAPTER_MAX} for core/fx`
    : null;
}

/** 64 floats (32 left, 32 right) of block k of a test signal, within +-1.0 like the clipped master. */
export function signalBlock(name: string, k: number, rng: PyRandom): number[] {
  const out: number[] = [];
  for (let ch = 0; ch < 2; ch++)
    for (let i = 0; i < 32; i++) {
      const n = k * 32 + i;
      let x: number;
      switch (name) {
        case 'silence':
          x = 0.0;
          break;
        case 'dc':
          x = ch === 0 ? 0.5 : -0.25;
          break;
        case 'square':
          x = Math.floor(n / 5) % 2 === ch ? 1.0 : -1.0;
          break;
        case 'noise':
          x = rng.uniform(-1.0, 1.0);
          break;
        case 'impulse':
          x = (n === 0 && ch === 0) || (n === 16 && ch === 1) ? 1.0 : 0.0;
          break;
        case 'sine':
          x = 0.9 * Math.sin((2 * Math.PI * (220 + 110 * ch) * n) / 48000);
          break;
        case 'ramp':
          x = ((n % 64) / 32.0 - 1.0) * (ch === 0 ? 1 : -1);
          break;
        default:
          throw new Error(`unknown signal ${name}`);
      }
      out.push(w2f(f2w(x)));
    }
  return out;
}
