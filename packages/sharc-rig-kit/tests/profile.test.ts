import { describe, expect, it } from 'vitest';
import { moduleBaseOf, rigAddrMap, rigProfile, sumCounts } from '../src/profile.ts';

// adapter code at 0x100..0x200 (_wrap), module code at 0x200..0x240 (_m_a, _m_loop; _m_alias shares _m_loop's address)
const built = {
  symbols: { _wrap: 0x100, _wrap_tail: 0x180, _m_a: 0x200, _m_loop: 0x210, _m_alias: 0x210 },
  moduleCode: [[0x200, 0x240]] as [number, number][],
};

describe('rig profile producer', () => {
  it('adds the counts of several calls per pc', () => {
    const sum = sumCounts([
      [
        [0x10, 1],
        [0x11, 2],
      ],
      [
        [0x11, 3],
        [0x20, 4],
      ],
    ]);
    expect([...sum]).toEqual([
      [0x10, 1],
      [0x11, 5],
      [0x20, 4],
    ]);
  });

  it('writes dk.profile/1 with pc counters in pc order, the blocks metric and the module load', () => {
    const p = rigProfile(
      { module: 'acme/demo', device: 'dt2', caseName: 'sample/a/note60', blocks: 3 },
      new Map([
        [0x211, 6],
        [0x201, 2],
      ]),
      moduleBaseOf(built),
    ) as Record<string, any>;
    expect(p.schema).toBe('dk.profile/1');
    expect(p.raw).toEqual([
      { cpu: 'sharc', addr: 0x201, count: 2, series: 'pc' },
      { cpu: 'sharc', addr: 0x211, count: 6, series: 'pc' },
    ]);
    expect(p.metrics).toEqual([{ name: 'sharc.blocks', value: 3, unit: 'blocks' }]);
    expect(p.run.loads.modules).toEqual([{ id: 'acme/demo', base: 0, size: 0, dsp_base: 0x200 }]);
    expect(p.clock.unit).toBe('instructions');
  });

  it('maps the adapter labels absolutely and the module labels relative to its image, one name per address', () => {
    const map = rigAddrMap(built, 'acme/demo', 'dt2') as { entries: Record<string, unknown>[] };
    expect(map.entries).toEqual([
      { cpu: 'sharc', addr: 0x100, size: 0x80, name: '_wrap', layer: 'our' },
      { cpu: 'sharc', addr: 0x180, size: 0x80, name: '_wrap_tail', layer: 'our' },
      { cpu: 'sharc', addr: 0, size: 0x10, name: '_m_a', layer: 'our', module: 'acme/demo' },
      { cpu: 'sharc', addr: 0x10, size: 0x30, name: '_m_alias', layer: 'our', module: 'acme/demo' },
    ]);
  });
});
