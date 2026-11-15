// The Profile pane's pure logic (src/profile/profile-core.ts): shares from the elapsed window, rows, names, the hot rule, the graph history.
import assert from 'node:assert/strict';
import { describe, test } from 'vitest';
import {
  buildRows,
  createHistory,
  dspText,
  dspView,
  dspWidth,
  fmtElapsed,
  fmtMhz,
  fmtShare,
  graphTop,
  groupRows,
  isHot,
  readReference,
  shareOf,
  totalShare,
  type ModuleRef,
  type Snapshot,
  type SpanIn,
} from '../src/profile/profile-core';

const HZ = 132_000_000;
const MODS: ModuleRef[] = [
  { index: 0, name: 'test/saw', kind: 'machine' },
  { index: 1, name: 'test/verb', kind: 'effect' },
  { index: 2, name: 'test/mon', kind: 'tool' },
];
const span = (kind: SpanIn['kind'], index: number, calls: number, sum: number, max: number): SpanIn => ({
  row: index,
  kind,
  index,
  calls,
  sumTicks: BigInt(sum),
  maxTicks: max,
});
const snap = (elapsed: number, spans: SpanIn[], running = true, polls?: Snapshot['polls']): Snapshot => ({
  running,
  hz: HZ,
  elapsedTicks: BigInt(elapsed),
  spans,
  polls,
});

describe('rows', () => {
  test('share is sum over the elapsed window, mean and max are in microseconds', () => {
    // 1 second: pluck 18% of it in 1000 calls
    const rows = buildRows(snap(HZ, [span('module', 0, 1000, Math.round(HZ * 0.18), Math.round(HZ * 210e-6))]), MODS);
    assert.equal(rows.length, 1);
    assert.equal(rows[0]!.name, 'test/saw');
    assert.equal(rows[0]!.kind, 'machine');
    assert.equal(fmtShare(rows[0]!.share), '18.0%');
    assert.ok(Math.abs(rows[0]!.meanUs - 180) < 0.01);
    assert.ok(Math.abs(rows[0]!.maxUs - 210) < 0.01);
  });

  test('owners that never ran are left out; names come from the module list, the phase table and the poll slots', () => {
    const rows = buildRows(
      snap(
        HZ,
        [
          span('module', 0, 0, 0, 0),
          span('module', 5, 2, 2000, 1500),
          span('phase', 1, 3, 300, 100),
          span('poll', 0, 3, 300, 100),
          span('poll', 1, 3, 300, 100),
          span('poll', 2, 3, 300, 100),
          span('poll', 3, 3, 300, 100),
          span('poll', 4, 3, 300, 100),
        ],
        true,
        [
          { owner: 1, fn: 0x100, name: 'tick' },
          { owner: 2, fn: 0x200 },
          { owner: 'runtime', fn: 0x300, name: 'usb_bulk_tick' },
          { owner: 'runtime', fn: 0x400 },
        ],
      ),
      MODS,
    );
    assert.deepEqual(
      rows.map((r) => [r.group, r.name, r.kind]),
      [
        ['modules', 'module 5', 'other'],
        ['phases', 'late', 'runtime'],
        ['polls', 'test/verb poll: tick', 'runtime'],
        ['polls', 'test/mon poll', 'runtime'],
        ['polls', 'runtime poll: usb_bulk_tick', 'runtime'],
        ['polls', 'runtime poll', 'runtime'],
        ['polls', 'poll 4', 'runtime'],
      ],
    );
  });

  test('rows are heaviest first and grouped in the panel order', () => {
    const rows = buildRows(
      snap(1_000_000, [
        span('poll', 0, 1, 10, 10),
        span('module', 1, 1, 50, 50),
        span('module', 0, 1, 90, 90),
        span('phase', 0, 1, 20, 20),
      ]),
      MODS,
    );
    assert.deepEqual(
      groupRows(rows).map((g) => [g.label, g.rows.map((r) => r.name)]),
      [
        ['Modules', ['test/saw', 'test/verb']],
        ['Runtime phases', ['early']],
        ['Polls', ['poll 0']],
      ],
    );
  });

  test('a window of zero ticks gives no share', () => {
    assert.equal(shareOf(100n, 0n), 0);
    assert.equal(buildRows(snap(0, [span('module', 0, 1, 100, 100)]), MODS)[0]!.share, 0);
  });

  test('64-bit sums do not lose the share', () => {
    const big = 2n ** 40n;
    assert.equal(shareOf(big / 4n, big), 0.25);
  });

  test('the whole window total adds the owners', () => {
    assert.equal(
      fmtShare(totalShare(snap(1000, [span('module', 0, 1, 100, 100), span('phase', 0, 1, 50, 50)]))),
      '15.0%',
    );
  });
});

describe('the hot rule', () => {
  test('a maximum more than 4 times the mean and at least 100 microseconds is marked', () => {
    assert.equal(isHot(49.2, 388), true);
    assert.equal(isHot(148.4, 210), false); // 1.4 times
    assert.equal(isHot(4.8, 61), false); // 12 times, but under 100 us
    assert.equal(isHot(25, 100), false); // exactly 4 times
    assert.equal(isHot(24, 100), true);
  });
});

describe('the history', () => {
  const at = (t: number, pluckSum: number, fxSum = 0): [Snapshot, ModuleRef[]] => [
    snap(t, [span('module', 0, 1, pluckSum, 1), span('module', 1, 1, fxSum, 1)]),
    MODS,
  ];

  test('each reading after the first makes one column of the interval share, by kind', () => {
    const h = createHistory();
    h.push(...at(1000, 100));
    h.push(...at(2000, 400, 100)); // interval: 1000 ticks, pluck +300, fx +100
    const [c] = h.columns();
    assert.equal(h.columns().length, 1);
    assert.equal(c!.machine, 0.3);
    assert.equal(c!.effect, 0.1);
    assert.equal(h.now(), 0.4);
  });

  test('only the newest columns are kept, and markers outside the window go', () => {
    const h = createHistory(3);
    for (let i = 0; i <= 5; i++) h.push(...at(1000 * i, 100 * i));
    assert.equal(h.columns().length, 3);
    assert.deepEqual(h.markers(), []); // the start marker is out of the window
  });

  test('a start and a module that appears are markers at their columns', () => {
    const h = createHistory();
    h.push(...at(1000, 0));
    h.push(...at(2000, 100));
    const [s, m] = at(3000, 200);
    h.push(s, [...m, { index: 3, name: 'test/echo', kind: 'effect' }]);
    assert.deepEqual(h.markers(), [
      { col: 0, text: 'start' },
      { col: 1, text: 'echo loaded' },
    ]);
  });

  test('a reading that went back (a reset) starts the graph again', () => {
    const h = createHistory();
    h.push(...at(1000, 100));
    h.push(...at(2000, 200));
    h.push(...at(500, 10));
    assert.equal(h.columns().length, 0);
    h.push(...at(1500, 60));
    assert.equal(h.columns().length, 1);
    assert.deepEqual(h.markers(), [{ col: 0, text: 'start' }]);
  });

  test('a reading with no new ticks (stopped) adds no column', () => {
    const h = createHistory();
    h.push(...at(1000, 100));
    h.push(...at(1000, 100));
    assert.equal(h.columns().length, 0);
  });

  test('the top of the graph is the first of 10, 25, 50 and 100 percent that holds the peak', () => {
    const col = (machine: number) => ({ machine, effect: 0, tool: 0, runtime: 0, other: 0 });
    assert.equal(graphTop([col(0.05)]), 0.1);
    assert.equal(graphTop([col(0.4)]), 0.5);
    assert.equal(graphTop([col(0.7)]), 1);
    assert.equal(graphTop([]), 0.1);
  });
});

describe('text and reference', () => {
  test('elapsed and clock', () => {
    assert.equal(fmtElapsed(BigInt(HZ) * 130n, HZ), '02:10');
    assert.equal(fmtMhz(132_000_000), '132 MHz');
    assert.equal(fmtMhz(133_333_333), '133.33 MHz');
  });

  test('a reference profile gives shares by name; one without spans or window gives none', () => {
    const doc = {
      schema: 'dk.profile/1',
      span_elapsed_ticks: 1000,
      spans: [
        { name: 'test/saw', sum_ticks: 250 },
        { name: 'early', sum_ticks: 10 },
      ],
    };
    assert.deepEqual(
      [...readReference(doc)!],
      [
        ['test/saw', 0.25],
        ['early', 0.01],
      ],
    );
    assert.equal(readReference({ schema: 'dk.profile/1', spans: [] }), undefined);
    assert.equal(readReference({ schema: 'dk.profile/1', spans: [{ name: 'x', sum_ticks: 1 }] }), undefined);
    assert.throws(() => readReference({ schema: 'other' }), /dk\.profile\/1/);
  });
});

describe('the DSP view', () => {
  const report = {
    blocks: 1000,
    series: [
      {
        series: 'pc',
        cpu: 'sharc',
        owners: [
          { owner: 'firmware', per_block: 50_000 },
          { owner: 'test/verb', per_block: 12_100 },
          { owner: 'test/saw', per_block: 31_600 },
          { owner: 'digikit/unloaded', per_block: 5 },
        ],
        functions: [
          { name: 'render', module: 'test/saw', per_block: 24_648 },
          { name: 'setup', module: 'test/saw', per_block: 4_740 },
          { name: 'note_on', module: 'test/saw', per_block: 2_212 },
          { name: 'extra', module: 'test/saw', per_block: 1 },
          { name: 'send', module: 'test/verb', per_block: 12_100 },
          { name: 'stock_fn', module: undefined, per_block: 40_000 },
        ],
      },
      { series: 'pc', cpu: 'cf', owners: [{ owner: 'test/saw', per_block: 1 }] },
    ],
  };

  test('loaded modules only, heaviest first, with their top three functions as a percent of the module', () => {
    const v = dspView(report, MODS, 92_796);
    assert.deepEqual(
      v.modules.map((m) => [m.name, m.insnPerBlock]),
      [
        ['test/saw', 31_600],
        ['test/verb', 12_100],
      ],
    );
    assert.deepEqual(v.modules[0]!.top, [
      { label: 'render', pct: 78 },
      { label: 'setup', pct: 15 },
      { label: 'note_on', pct: 7 },
    ]);
    assert.equal(v.total, 43_700);
    assert.equal(v.blocks, 1000);
    assert.equal(dspText(v), '43,700 / 92,796');
    assert.ok(Math.abs(dspWidth(v, 31_600) - 34.05) < 0.01);
  });

  test('without a headroom the bar is the total and the text is the total alone', () => {
    const v = dspView(report, MODS);
    assert.equal(dspText(v), '43,700');
    assert.equal(dspWidth(v, 43_700), 100);
  });

  test('a report without a SHARC series is an empty view', () => {
    const v = dspView({ series: [] }, MODS, 100);
    assert.deepEqual(v.modules, []);
    assert.equal(dspWidth(v, 5), 5);
  });
});
