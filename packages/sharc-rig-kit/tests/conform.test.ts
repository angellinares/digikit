import { describe, expect, it } from 'vitest';
import { machineCases, type Case, fxCases } from '../src/cases.ts';
import { descriptionProblems, runFxCase, runMachineCase, runPoly } from '../src/conform.ts';
import { STOCK_BP, nw } from '../src/context.ts';
import { f2w } from '../src/context.ts';
import type { CallSpec, Op } from '../src/rig.ts';
import { fakeRig, fxBuilt, built, spec } from './fakes.ts';

const bud = { voiceBlock: 1000, noteOnBlock: 5000, headroom: 5000 };
const calls = (batches: Op[][]) => batches.flat().filter((o): o is { call: CallSpec } => 'call' in o);
const reads = (n: number, word: number) => Array<number>(n).fill(word);
const ok = (instructions: number) => ({ instructions, reads: [reads(32, f2w(0.5)), reads(32, 0), [0]] });
const oneCase = (over: Partial<Case>): Case => ({
  group: 'g',
  name: 'n',
  note: 60,
  vel: 100,
  words: [[['a', 1]]],
  blocks: 4,
  trig: [0],
  ...over,
});

describe('runMachineCase', () => {
  it('records the worst steady block and the worst note-on block, which on DN2 is the trigger block and the one after it', async () => {
    const counts = [900, 4000, 800, 700];
    const fake = fakeRig('dn2', (k) => ({ instructions: counts[k]!, reads: [reads(32, f2w(0.5)), [0]] }));
    const r = await runMachineCase(spec('dn2'), oneCase({}), bud, 1, { open: fake.open, built: built() });
    expect(r).toMatchObject({ blocks: 4, steady: 800, noteon: 4000, failure: null });
    expect(fake.closed()).toBe(true);
  });

  it('counts only the trigger block as a note-on block on DT2', async () => {
    const counts = [4000, 4500, 800, 700];
    const fake = fakeRig('dt2', (k) => ok(counts[k]!));
    const r = await runMachineCase(spec('dt2'), oneCase({}), bud, 1, { open: fake.open, built: built() });
    // block 1 is a steady block on DT2 and takes 4500 instructions: over the voice-block budget
    expect(r.failure).toBe(
      'test/fake@dt2 case g/n block 1: note 60 velocity 100 words [a=0x1]: 4500 instructions in a block, budget 1000 per voice',
    );
    expect(r).toMatchObject({ blocks: 2, steady: 4500, noteon: 4000 });
  });

  it('stops at the first failing block and asks for no more blocks', async () => {
    const fake = fakeRig('dt2', (k) =>
      k === 2 ? { halt: 'max-steps', final_pc: 3, reads: [reads(32, 0), reads(32, 0), [0]] } : ok(100),
    );
    const r = await runMachineCase(spec('dt2'), oneCase({ blocks: 50 }), bud, 1, { open: fake.open, built: built() });
    expect(r.failure).toContain('block 2: note 60 velocity 100 words [a=0x1]: the call did not return');
    expect(r.blocks).toBe(3);
    expect(calls(fake.batches)).toHaveLength(3);
    expect(fake.closed()).toBe(true);
  });

  it('retriggers at the blocks of the case', async () => {
    const fake = fakeRig('dt2', () => ok(100));
    await runMachineCase(spec('dt2'), oneCase({ blocks: 6, trig: [0, 3] }), bud, 1, {
      open: fake.open,
      built: built(),
    });
    const trigWrites = fake.batches.map((ops) =>
      ops.some((o) => 'poke' in o && o.poke.some(([a, w]) => a === nw(0x20093000 + 0x1b8) && w === 1)),
    );
    expect(trigWrites).toEqual([true, false, false, true, false, false]);
  });

  it('publishes the sample for the "sample-pool" setup before the first block, the ready word last', async () => {
    const fake = fakeRig('dt2', () => ok(100));
    const sp = { ...spec('dt2'), sampleRecords: '_pool' };
    const b = { ...built(), data: { ...built().data, _pool: 0x200c0000 } };
    await runMachineCase(sp, oneCase({ setup: 'sample-pool', sample: [64, 48000, 1], blocks: 1 }), bud, 1, {
      open: fake.open,
      built: b,
    });
    const pokes = fake.batches[0]!.flatMap((o) => ('poke' in o ? o.poke : []));
    const rec = nw(0x200c0000);
    const record = pokes.filter(([a]) => a >= rec && a < rec + 8);
    expect(record.at(-1)).toEqual([rec, 1]);
    expect(record.find(([a]) => a === rec + 2)![1]).toBe(64); // frames
    expect(record.find(([a]) => a === rec + 4)![1]).toBe(1); // channels
    expect(pokes.some(([a]) => a === nw(0x200f0000))).toBe(true); // the first plane word
  });

  it('runs the real case list through the fake: every case starts a fresh rig', async () => {
    const fake = fakeRig('dt2', () => ok(100));
    const cases = machineCases(spec('dt2')).slice(0, 3);
    for (const c of cases) await runMachineCase(spec('dt2'), c, bud, 1, { open: fake.open, built: built() });
    expect(fake.setups).toHaveLength(3);
  });
});

describe('runPoly', () => {
  const run = async (reply: Parameters<typeof fakeRig>[1], n = 0) => {
    const fake = fakeRig('dt2', reply);
    const r = await runPoly(spec('dt2'), [24, 60], 1, { open: fake.open, built: built() });
    return { r, fake, n };
  };

  it('runs voice 0 for 12 blocks and voice 1 for 8, on one core', async () => {
    const { r, fake } = await run(() => ok(100));
    expect(r).toMatchObject({ group: 'writes', name: 'two-voices/notes24,60', blocks: 12, failure: null });
    expect(calls(fake.batches)).toHaveLength(12 + 8);
    expect(fake.setups).toHaveLength(1);
  });

  it('fails on a store of one voice outside its own memory, naming the block and the voice', async () => {
    const { r } = await run((k) => ({
      ...ok(100),
      ...(k === 5 ? { stores: [[0x1010, 0x20091000, 4] as [number, number, number]] } : {}),
    }));
    expect(r.failure).toMatch(
      /^test\/fake@dt2 case writes\/two-voices\/notes24,60 block \d+ voice [01]: the module wrote memory it does not own \(in render/,
    );
    expect(r.blocks).toBeLessThan(12);
  });

  it('fails on a call that did not return, with a text that says so', async () => {
    const { r } = await run((k) => (k === 1 ? { halt: 'max-steps' } : ok(100)));
    expect(r.failure).toContain('(the call did not return)');
  });
});

describe('runFxCase', () => {
  const desc = [0x444b4658, 1, 2, 8, 0, 64];
  const sp = {
    module: 'test/fxx',
    moddir: '/x',
    sources: [],
    process: '_te_half',
    desc: '_te_desc',
    state: '_te_state_a',
    knobs: [] as [number, number][],
  };
  const silence = fxCases(2)[0]!;
  const reply = (over: (k: number) => object) => (k: number) => ({
    final_pc: STOCK_BP,
    instructions: 300,
    reads: [reads(64, f2w(0.5)), [0], [0], [0], [0], reads(64, f2w(0.5))],
    ...over(k),
  });

  it('runs every block of the case and reports the worst call', async () => {
    const fake = fakeRig(
      'dt2',
      reply((k) => ({ instructions: 300 + k })),
    );
    const r = await runFxCase(sp, silence, 1000, desc, 1, { open: fake.open, built: fxBuilt() });
    expect(r).toMatchObject({ group: 'signals', name: 'silence', blocks: 24, steady: 323, noteon: 0, failure: null });
  });

  it('fails when the adapter guard stepped in, and compares the fault count with its value in the previous block', async () => {
    const guard = fakeRig(
      'dt2',
      reply((k) => (k >= 2 ? { reads: [reads(64, 0), [1], [0], [0], [0], reads(64, 0)] } : {})),
    );
    const a = await runFxCase(sp, silence, 1000, desc, 1, { open: guard.open, built: fxBuilt() });
    expect(a.failure).toContain("block 2: signal silence words [p0=0x0 p1=0x0]: the adapter's guard stepped in");
    expect(a.blocks).toBe(3);
    // a count that stays at 1 after the first refusal is not a new refusal
    const steady = fakeRig(
      'dt2',
      reply(() => ({ reads: [reads(64, 0), [1], [0], [0], [0], reads(64, 0)] })),
    );
    const b = await runFxCase(sp, silence, 1000, desc, 1, { open: steady.open, built: fxBuilt() });
    expect(b.failure).toContain('block 0');
    expect(b.blocks).toBe(1);
  });

  it('holds a call to the effect budget plus the adapter budget, and counts the block that broke it as the worst', async () => {
    const fake = fakeRig(
      'dt2',
      reply((k) => ({ instructions: k === 1 ? 2000 : 300 })),
    );
    const r = await runFxCase(sp, silence, 1000, desc, 1, { open: fake.open, built: fxBuilt() });
    expect(r.failure).toContain('2000 instructions in a block, budget 1000 for the effect and 900 for core/fx');
    expect(r.steady).toBe(2000);
  });

  it('switches the effect off and on at the toggle blocks', async () => {
    const fake = fakeRig(
      'dt2',
      reply(() => ({})),
    );
    const toggle = fxCases(2).find((c) => c.name === 'toggle-every-1/square')!;
    await runFxCase(sp, { ...toggle, blocks: 4 }, 1000, desc, 1, { open: fake.open, built: fxBuilt() });
    const enabled = fake.batches.map((ops) =>
      ops
        .flatMap((o) => ('poke' in o ? o.poke : []))
        .filter(([a]) => a === nw(0x2009a000) + 3)
        .map(([, w]) => w),
    );
    expect(enabled).toEqual([[], [0], [1], [0]]);
  });
});

describe('descriptionProblems', () => {
  it('names every way a description can be wrong', () => {
    expect(descriptionProblems([0x444b4658, 1, 8, 16])).toEqual([]);
    expect(descriptionProblems([0, 2, 9, 5])).toEqual([
      'the description has no DK_FX_MAGIC',
      'contract 2 is not DK_FX_V1',
      '9 parameters, at most 8',
      'state_bytes 5 is not a multiple of 4',
    ]);
  });
});
