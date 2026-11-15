// The dk.profile/1 builders (src/profile/profile-artifact.ts) that `wb profile ...` and the Profile pane share.
import assert from 'node:assert/strict';
import { test } from 'vitest';
import { buildProfile, cfPart, runId, sharcPart, spansPart } from '../src/profile/profile-artifact';

const spans = spansPart(
  {
    hz: 132_000_000,
    elapsedTicks: 2n ** 33n,
    spans: [
      { kind: 'module', index: 2, calls: 5, sumTicks: 2n ** 33n, maxTicks: 9 },
      { kind: 'phase', index: 1, calls: 1, sumTicks: 3n, maxTicks: 3 },
      { kind: 'poll', index: 0, calls: 0, sumTicks: 0n, maxTicks: 0 },
    ],
  },
  (s) => `${s.kind}${s.index}`,
);

test('spans carry the owner id, the clock and the elapsed window', () => {
  assert.deepEqual(
    spans.spans.map((s) => [s.owner, s.kind, s.name, s.sum_ticks]),
    [
      [2, 'module', 'module2', 2 ** 33],
      [0x101, 'phase', 'phase1', 3],
      [0x200, 'poll', 'poll0', 0],
    ],
  );
  assert.equal(spans.span_clock_hz, 132_000_000);
  assert.equal(spans.span_elapsed_ticks, 2 ** 33);
});

test('spans alone are an artifact with the runtime.span clock and no raw rows', () => {
  const p = buildProfile({ run: { id: 'x', target: 'dn2' }, spans });
  assert.equal(p.schema, 'dk.profile/1');
  assert.equal((p.clock as { source: string }).source, 'runtime.span');
  assert.deepEqual(p.raw, []);
  assert.deepEqual(p.markers, []);
  assert.equal(p.span_elapsed_ticks, 2 ** 33);
});

test('an empty span table leaves the span fields out', () => {
  const p = buildProfile({ run: {}, spans: { ...spans, spans: [] } });
  assert.equal('spans' in p, false);
});

test('cf.profile is the difference of two answers, per series and path', () => {
  const before = {
    pc_samples: [['100', 5]] as [string, number][],
    paths: [{ path: 'run_fast', instructions: 10, calls: 1, host_ns: 5 }],
  };
  const after = {
    pc_samples: [
      ['100', 8],
      ['200', 4],
    ] as [string, number][],
    idle_pc: [['300', 2]] as [string, number][],
    paths: [{ path: 'run_fast', instructions: 30, calls: 3, host_ns: 15 }],
  };
  const part = cfPart(before, after);
  assert.deepEqual(part.raw, [
    { cpu: 'cf', addr: 100, count: 3, series: 'pc' },
    { cpu: 'cf', addr: 200, count: 4, series: 'pc' },
    { cpu: 'cf', addr: 300, count: 2, series: 'idle_pc' },
  ]);
  const m = (n: string) => part.metrics.find((x) => x.name === n)?.value;
  assert.equal(m('path.run_fast.instructions'), 20);
  assert.equal(m('path.ram_clear.calls'), 0);
  // without a "before" the state since boot is kept
  assert.equal(cfPart(undefined, after).raw.length, 3);
});

test('both CPUs and the spans make one artifact; the clock names both sources', () => {
  const sharc = sharcPart({
    clock: { source: 'emu.sharc.insn', unit: 'insn', sampling: 's' },
    blocks: 10,
    icount: 1,
    idle_skipped: 0,
    counted: { fast: 1, block: 0, interpreted: 0 },
    raw: [{ cpu: 'sharc', addr: 1, count: 2, series: 'pc' }],
  });
  const cf = cfPart(undefined, { paths: [] });
  const p = buildProfile({ run: { id: 'x' }, cf, sharc, spans });
  assert.equal((p.clock as { source: string }).source, 'emu.cf.insn+emu.sharc.insn');
  assert.equal((p.raw as unknown[]).length, 1);
  assert.deepEqual(p.markers, [{ name: 'record.start', at: 0 }]);
  assert.ok((p.metrics as { name: string }[]).some((x) => x.name === 'sharc.blocks'));
  assert.ok(Array.isArray(p.spans));
});

test('run ids are the date and time', () => {
  assert.equal(runId('emu', new Date('2026-11-15T12:34:56Z')), 'emu-20261115T123456');
});
