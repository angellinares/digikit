import { describe, expect, it } from 'vitest';
import { LONG_BLOCKS, RETRIG_BLOCKS, WALK_BLOCKS, fxCases, machineCases, wordsAt } from '../src/cases.ts';
import { FX_PARAM_MAX } from '../src/spec.ts';
import { param, spec } from './fakes.ts';

describe('machineCases', () => {
  const sp = spec('dt2', [param('a', 1, 0, 100, 50), param('b', 2, 10, 20, 10)]);
  const cases = machineCases(sp);
  const names = (g: string) => cases.filter((c) => c.group === g).map((c) => c.name);

  it('runs every parameter at its limits per note, skipping a value that is the default', () => {
    const at60 = names('limits').filter((n) => n.endsWith('/note60'));
    // a: min, max (mid 48 differs from the default 50); b: max and mid (min is the default 10)
    expect(at60).toEqual([
      'all-default/note60',
      'a=min/note60',
      'a=max/note60',
      'a=mid/note60',
      'b=max/note60',
      'b=mid/note60',
    ]);
    expect(cases.filter((c) => c.group === 'limits').every((c) => c.blocks === 4 && c.trig.join() === '0')).toBe(true);
  });

  it('runs all parameter corners for three notes', () => {
    expect(names('corners')).toHaveLength(3 * 4);
    expect(names('corners')).toContain('corner-HL/note127');
  });

  it('moves every parameter every block in the modulation cases, inside its range', () => {
    const walk = cases.find((c) => c.name === 'random-walk/note24')!;
    expect(walk.blocks).toBe(WALK_BLOCKS);
    for (let k = 0; k < WALK_BLOCKS; k++)
      for (const [id, w] of wordsAt(walk, k)) {
        const p = sp.params.find((x) => x.id === id)!;
        expect(w).toBeGreaterThanOrEqual(p.lo);
        expect(w).toBeLessThanOrEqual(p.hi);
      }
    // block 1: parameter 0 at its maximum, parameter 1 at its minimum (the parity of block plus index)
    expect(cases.find((c) => c.name === 'square-every-block/note60')!.words[1]).toEqual([
      ['a', 100],
      ['b', 10],
    ]);
  });

  it('draws the same walk for the same seed', () => {
    expect(machineCases(sp).find((c) => c.name === 'random-walk/note100')!.words).toEqual(
      cases.find((c) => c.name === 'random-walk/note100')!.words,
    );
  });

  it('adds a long note and retriggers every 7th block at both velocity limits', () => {
    expect(cases.find((c) => c.group === 'long')!.blocks).toBe(LONG_BLOCKS);
    const retrig = cases.filter((c) => c.group === 'retrigger');
    expect(retrig.map((c) => c.vel)).toEqual([1, 127]);
    expect(retrig[0]!.blocks).toBe(RETRIG_BLOCKS);
    expect(retrig[0]!.trig.slice(0, 3)).toEqual([0, 7, 14]);
    expect(retrig[0]!.trig.at(-1)).toBe(273);
  });

  it('has the sample group only for a machine with the sample-pool fixture and the parameters it plays with', () => {
    expect(cases.some((c) => c.group === 'sample')).toBe(false);
    const ids = ['sample', 'start', 'end', 'play', 'pitch'];
    const player = {
      ...spec(
        'dt2',
        ids.map((id, i) => param(id, i, 0, 0x7f00, 0)),
      ),
      fixtures: ['sample-pool'],
      sampleRecords: '_pool',
    };
    const sample = machineCases(player).filter((c) => c.group === 'sample');
    expect(sample.length).toBeGreaterThan(8);
    expect(sample.every((c) => c.setup === 'sample-pool' && c.sample !== undefined)).toBe(true);
    // the sample index stays 0 in the walk, so the published pool entry stays selected
    const walk = sample.find((c) => c.name === 'random-walk/note24')!;
    expect(walk.words.every((row) => row[0]![0] === 'sample' && row[0]![1] === 0 && row.length === 5)).toBe(true);
    expect(machineCases({ ...player, fixtures: [] }).some((c) => c.group === 'sample')).toBe(false);
  });
});

describe('fxCases', () => {
  const cases = fxCases(2);

  it('runs the test signals, the parameter limits, the modulation, the switching and a long run', () => {
    expect(cases.filter((c) => c.group === 'signals').map((c) => c.name)).toEqual([
      'silence',
      'dc',
      'square',
      'noise',
      'impulse',
      'sine',
      'ramp',
    ]);
    expect(cases.filter((c) => c.group === 'params').map((c) => c.name)).toContain('p1=max/noise');
    expect(cases.filter((c) => c.group === 'params')).toHaveLength(6 + 4);
    expect(cases.find((c) => c.name === 'toggle-every-7/noise')!.toggle).toBe(7);
    expect(cases.find((c) => c.group === 'long')!.blocks).toBe(LONG_BLOCKS);
  });

  it('keeps the walk words inside the parameter range', () => {
    const walk = cases.find((c) => c.name === 'random-walk/noise')!;
    expect(walk.words).toHaveLength(WALK_BLOCKS);
    expect(walk.words.flat().every((w) => w >= 0 && w <= FX_PARAM_MAX)).toBe(true);
  });

  it('puts the parameter under test at its limit and the others at mid', () => {
    expect(cases.find((c) => c.name === 'p0=min/noise')!.words).toEqual([[0, 0x3f00]]);
  });
});
