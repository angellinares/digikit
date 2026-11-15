import { describe, expect, it } from 'vitest';
import {
  describeFx,
  describeMachine,
  fxBlockFailure,
  fxBudgetFailure,
  machineBlockFailure,
  machineBudgetFailure,
  pyRepr,
  signalBlock,
} from '../src/checks.ts';
import { f2w } from '../src/context.ts';
import type { FxBlock } from '../src/fx.ts';
import type { MachineBlock } from '../src/machine.ts';
import { PyRandom } from '../src/prng.ts';

const good = (): MachineBlock => ({
  samples: Array<number>(32).fill(0.25),
  instructions: 1000,
  returned: true,
  halt: 'breakpoint',
  finalPc: 0xb9f100,
  contextBad: [],
  faults: 0,
  foreign: [],
  right: [],
  active: 0,
  extra: [],
  profile: [],
});
const where = 'case g/n block 3: note 60 velocity 100 words [a=0x10]';
const failure = (b: Partial<MachineBlock>) => machineBlockFailure(where, { ...good(), ...b }, 400_000, { M5: -2 });

describe('machineBlockFailure', () => {
  it('passes a block that returned, kept the context, wrote its own memory and gave finite samples inside the bound', () => {
    expect(failure({})).toBeNull();
  });

  it('names a call that did not return, with its halt and pc', () => {
    expect(failure({ returned: false, halt: 'max-steps', finalPc: 0x1234, instructions: 400_000 })).toBe(
      `${where}: the call did not return to the caller within 400000 steps (halt max-steps at 0x1234, 400000 instructions)`,
    );
  });

  it('reports the first broken rule when several are broken: returned, guard, writes, context, finite, bound', () => {
    const all = { faults: 2, foreign: ['store'], contextBad: ['M5'], samples: Array<number>(32).fill(Number.NaN) };
    expect(failure({ ...all, returned: false })).toContain('did not return');
    expect(failure(all)).toContain('the runtime guard stepped in (2 fault(s) counted');
    expect(failure({ ...all, faults: 0 })).toContain('the module wrote memory it does not own (store)');
    expect(failure({ ...all, faults: 0, foreign: [] })).toContain(
      'the context registers M5 did not come back as they went in (in M5=-0x2)',
    );
    expect(failure({ ...all, faults: 0, foreign: [], contextBad: [] })).toContain(
      'output sample 0 of 32 is nan (not finite',
    );
  });

  it('names the first sample that is not finite or beyond the level bound', () => {
    const s = Array<number>(32).fill(0);
    s[7] = Number.POSITIVE_INFINITY;
    expect(failure({ samples: s })).toContain('output sample 7 of 32 is inf');
    s[7] = -8.5;
    expect(failure({ samples: s })).toBe(`${where}: output sample 7 is -8.5, above the level bound 8.0`);
    s[7] = 8; // the bound itself is allowed
    expect(failure({ samples: s })).toBeNull();
  });
});

describe('machineBudgetFailure', () => {
  const bud = { voiceBlock: 1000, noteOnBlock: 5000, headroom: 5000 };

  it('holds a note-on block to the note-on budget and every other block to the voice-block budget', () => {
    expect(machineBudgetFailure(where, 1000, false, bud)).toBeNull();
    expect(machineBudgetFailure(where, 1001, false, bud)).toBe(
      `${where}: 1001 instructions in a block, budget 1000 per voice`,
    );
    expect(machineBudgetFailure(where, 5000, true, bud)).toBeNull();
    expect(machineBudgetFailure(where, 5001, true, bud)).toBe(
      `${where}: 5001 instructions in a note-on block, budget 5000`,
    );
  });
});

const fxGood = (): FxBlock => ({
  master: Array<number>(64).fill(0.5),
  ret: [],
  instructions: 500,
  returned: true,
  halt: 'breakpoint',
  finalPc: 0xb9f000,
  ctxBad: [],
  regsBad: [],
  faults: [0, 0, 0, 0],
  scratch: Array<number>(64).fill(0.5),
  stores: [],
  extra: [],
});
const fxFailure = (b: Partial<FxBlock>, before = 0, foreign: string[] = []) =>
  fxBlockFailure(where, { ...fxGood(), ...b }, 200_000, before, () => foreign);

describe('fxBlockFailure', () => {
  it('passes a block that returned, kept its registers, did not trip the adapter guard and left a bounded master', () => {
    expect(fxFailure({})).toBeNull();
  });

  it('names the adapter guard by the output sample that made it refuse the block, or by a negative return', () => {
    const scratch = Array<number>(64).fill(0);
    scratch[40] = Number.NaN;
    expect(fxFailure({ faults: [1, 0, 0, 0], scratch })).toBe(
      `${where}: the adapter's guard stepped in (output sample 40 of 64 is nan; the block played dry)`,
    );
    expect(fxFailure({ faults: [3, 0, 0, 0] }, 2)).toContain('the process routine returned a negative number');
    expect(fxFailure({ faults: [3, 0, 0, 0] }, 3)).toBeNull(); // the count is compared with its value before the call
  });

  it('reports registers, then stores outside the write set, then the master', () => {
    const master = Array<number>(64).fill(0);
    master[2] = 9;
    expect(fxFailure({ ctxBad: ['M5'], regsBad: ['R3'], master }, 0, ['s'])).toContain(
      'registers M5,R3 did not come back',
    );
    expect(fxFailure({ master }, 0, ['s1', 's2'])).toContain('the module wrote memory it does not own (s1; s2)');
    expect(fxFailure({ master })).toBe(`${where}: master sample 2 is 9.0 after the hook`);
  });

  it('names a hook that did not return', () => {
    expect(fxFailure({ returned: false, halt: 'trap: x', finalPc: 5, instructions: 7 })).toBe(
      `${where}: the hook did not return to the stock function within 200000 steps (halt trap: x at 0x5, 7 instructions)`,
    );
  });
});

describe('fxBudgetFailure', () => {
  it('allows the effect budget plus the 900 instructions of core/fx', () => {
    expect(fxBudgetFailure(where, 1900, 1000)).toBeNull();
    expect(fxBudgetFailure(where, 1901, 1000)).toBe(
      `${where}: 1901 instructions in a block, budget 1000 for the effect and 900 for core/fx`,
    );
  });
});

describe('messages', () => {
  it('shows the parameter words in hex and says when there are none', () => {
    expect(
      describeMachine(
        'limits',
        'x',
        2,
        [
          ['a', 255],
          ['b', 0],
        ],
        60,
        100,
      ),
    ).toBe('case limits/x block 2: note 60 velocity 100 words [a=0xff b=0x0]');
    expect(describeMachine('limits', 'x', 2, [], 60, 100)).toContain('words [(no parameters)]');
    expect(describeFx('signals', 'dc', 0, 'dc', [1, 16])).toBe(
      'case signals/dc block 0: signal dc words [p0=0x1 p1=0x10]',
    );
  });

  it('writes a float like Python', () => {
    expect([pyRepr(2), pyRepr(0.1), pyRepr(Number.NEGATIVE_INFINITY), pyRepr(Number.NaN)]).toEqual([
      '2.0',
      '0.1',
      '-inf',
      'nan',
    ]);
  });
});

describe('signalBlock', () => {
  const rng = () => new PyRandom(7);

  it('makes 32 left and 32 right samples that are float32 values within +-1', () => {
    for (const name of ['silence', 'dc', 'square', 'noise', 'impulse', 'sine', 'ramp']) {
      const b = signalBlock(name, 3, rng());
      expect(b).toHaveLength(64);
      for (const x of b) {
        expect(Math.abs(x)).toBeLessThanOrEqual(1);
        expect(Math.fround(x)).toBe(x);
      }
    }
  });

  it('puts the impulse at sample 0 of the left channel and sample 16 of the right channel of the first block only', () => {
    const b = signalBlock('impulse', 0, rng());
    expect(b.flatMap((x, i) => (x ? [i] : []))).toEqual([0, 32 + 16]);
    expect(signalBlock('impulse', 1, rng()).some((x) => x)).toBe(false);
  });

  it('holds a square for 5 samples and shifts it by one half period between the channels', () => {
    const b = signalBlock('square', 0, rng());
    expect(b.slice(0, 10)).toEqual([1, 1, 1, 1, 1, -1, -1, -1, -1, -1]);
    expect(b.slice(32, 42)).toEqual([-1, -1, -1, -1, -1, 1, 1, 1, 1, 1]);
  });

  it('draws the noise from the generator, one stream across the blocks', () => {
    const r = rng();
    const first = signalBlock('noise', 0, r);
    const second = signalBlock('noise', 1, r);
    expect(second).not.toEqual(first);
    expect(first[0]).toBe(Math.fround(new PyRandom(7).uniform(-1, 1)));
  });

  it('is exact for the signals that need no random draw', () => {
    expect(signalBlock('dc', 0, rng()).slice(0, 2)).toEqual([0.5, 0.5]);
    expect(signalBlock('dc', 0, rng())[32]).toBe(-0.25);
    expect(f2w(signalBlock('ramp', 0, rng())[0]!)).toBe(f2w(-1));
    expect(() => signalBlock('nope', 0, rng())).toThrow('unknown signal');
  });
});
