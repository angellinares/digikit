import type { Bar, KnownModule } from '../src/memory-core.ts';
import type { Caps } from '../../../web/src/modules/fit.ts';
// The Memory panel's model (apps/desktop/ui/src/memory-core.ts): one bar per resource the device limits, a segment per loaded module coloured by its
// type, "other" for the rest of what is used, and the texts.
import assert from 'node:assert/strict';
import { test } from 'vitest';
import {
  amountText,
  buildBars,
  freeTitle,
  noteText,
  kibText,
  segmentTitle,
  SPECS,
  usedText,
  widthPct,
} from '../src/memory-core.ts';

const KiB = 1024;
const mod = (name: string, group: KnownModule['group'], needs: KnownModule['needs']): KnownModule => ({
  name,
  label: name.split('/').pop()!,
  group,
  needs,
});
const library = [
  mod('test/sine', 'Machines', { ARENA: 4096, DSP_AREA: 259 * KiB, MACHINE_ROWS: 1, PARAM_ROWS: 4 }),
  mod('digikit/chorus', 'Effects', { ARENA: 2048 }),
  mod('digikit/remote', 'Tools', { ARENA: 1024 }),
  mod('digikit/broken', 'Tools', null), // no needs: the loader would refuse the file
];
const c = (limit: number, used: number) => ({ limit, used, largest: limit - used });
// a Digitone II: pages; the runtime's own blocks are in `used` too (6 KiB of the arena, nothing else)
const dn2: Caps = {
  MODULES: c(8, 3),
  ARENA: c(512 * KiB, 4096 + 2048 + 1024 + 6 * KiB),
  DSP_AREA: c(512 * KiB, 259 * KiB),
  MACHINE_ROWS: c(8, 1),
  PARAM_ROWS: c(16, 4),
  PAGE_ROWS: c(8, 0),
  VOICE_ROWS: c(8, 1),
  SELECTION: c(8, 3),
  STAGE: c(65536, 0),
};
const loaded = ['test/sine', 'digikit/chorus', 'digikit/remote'];
const by = (bars: Bar[], r: string) => bars.find((b) => b.resource === r)!;

test('one bar per resource the device limits, in the panel order, under the user-facing names', () => {
  const bars = buildBars(dn2, loaded, library);
  assert.deepEqual(
    bars.map((b) => b.label),
    ['DSP memory', 'CPU memory', 'Machines', 'Parameters', 'Pages', 'Modules'],
  );
  // the voice rows, the selection paths and the staging buffer are not shown
  assert.equal(by(bars, 'VOICE_ROWS'), undefined);
  assert.equal(by(bars, 'SELECTION'), undefined);
  assert.equal(by(bars, 'STAGE'), undefined);
  assert.deepEqual(
    SPECS.map((s) => s.bytes),
    [true, true, false, false, false, false],
  );
  for (const b of bars) assert.doesNotMatch(b.label, /arena|area/i);
});

test('a resource the device does not have is hidden: the Digitakt II has no page rows', () => {
  const dt2 = { ...dn2, PAGE_ROWS: c(0, 0) };
  const labels = buildBars(dt2, loaded, library).map((b) => b.label);
  assert.ok(!labels.includes('Pages'));
  assert.equal(labels.length, 5);
  // a capacity without the kind (an older runtime) has no bar either
  const older = { ...dn2 };
  delete older.PAGE_ROWS;
  delete older.MACHINE_ROWS;
  assert.deepEqual(
    buildBars(older, loaded, library).map((b) => b.resource),
    ['DSP_AREA', 'ARENA', 'PARAM_ROWS', 'MODULES'],
  );
});

test('each bar has a segment per loaded module that takes some of it, coloured by type, then other, then free', () => {
  const bars = buildBars(dn2, loaded, library);
  const dsp = by(bars, 'DSP_AREA');
  assert.deepEqual(
    dsp.segments.map((s) => [s.id, s.kind, s.amount]),
    [['test/sine', 'machine', 259 * KiB]], // the effect and the tool take none of the DSP memory: no segment
  );
  assert.equal(dsp.free, 253 * KiB);
  const cpu = by(bars, 'ARENA');
  assert.deepEqual(
    cpu.segments.map((s) => [s.label, s.kind, s.amount]),
    [
      ['sine', 'machine', 4096],
      ['chorus', 'effect', 2048],
      ['remote', 'tool', 1024],
      ['Other', 'other', 6 * KiB], // the runtime's own blocks: used, but no known module's
    ],
  );
  // the parts add up to the limit
  for (const b of bars) assert.equal(b.segments.reduce((n, s) => n + s.amount, 0) + b.free, b.limit);
  const mods = by(bars, 'MODULES');
  assert.deepEqual(
    mods.segments.map((s) => [s.label, s.amount]),
    [
      ['sine', 1],
      ['chorus', 1],
      ['remote', 1],
    ],
  );
  assert.deepEqual(
    by(bars, 'MACHINE_ROWS').segments.map((s) => s.label),
    ['sine'],
  );
  assert.deepEqual(by(bars, 'PAGE_ROWS').segments, []); // nothing uses a page row
});

test('a loaded module that is not in the library, or has no needs, is part of "other"', () => {
  const caps = { ...dn2, MODULES: c(8, 4), ARENA: c(512 * KiB, 4096 + 9000), MACHINE_ROWS: c(8, 2) };
  const bars = buildBars(caps, ['test/sine', 'someone/mystery', 'digikit/broken'], library);
  const mods = by(bars, 'MODULES');
  assert.deepEqual(
    mods.segments.map((s) => [s.id, s.kind, s.amount]),
    [
      ['test/sine', 'machine', 1],
      ['other', 'other', 3], // the unknown one, the one without needs and one the device holds that MODULES did not name
    ],
  );
  assert.deepEqual(
    by(bars, 'ARENA').segments.map((s) => [s.id, s.amount]),
    [
      ['test/sine', 4096],
      ['other', 9000],
    ],
  );
  assert.deepEqual(
    by(bars, 'MACHINE_ROWS').segments.map((s) => [s.id, s.amount]),
    [
      ['test/sine', 1],
      ['other', 1],
    ],
  );
});

test('nothing loaded: every bar is free, and a known module never takes more than the device says is used', () => {
  const idle = buildBars({ MODULES: c(8, 0), ARENA: c(512 * KiB, 0), DSP_AREA: c(512 * KiB, 0) }, [], library);
  for (const b of idle) {
    assert.deepEqual(b.segments, []);
    assert.equal(b.free, b.limit);
  }
  // the DSP image's arena block is freed after its transfer: a device that reports less used than the file's figures is not shown over its limit
  const tight = buildBars({ ...dn2, ARENA: c(512 * KiB, 3000) }, loaded, library);
  const cpu = by(tight, 'ARENA');
  assert.equal(cpu.used, 3000);
  assert.equal(
    cpu.segments.reduce((n, s) => n + s.amount, 0),
    3000,
  );
});

test('used above the limit is shown as over, with proportional segments', () => {
  const over = by(buildBars({ ARENA: c(1000, 1500) }, [], library), 'ARENA');
  assert.deepEqual([over.used, over.free, over.largest, over.over], [1500, 0, 0, true]);
  assert.equal(usedText(over), '1.5 KiB / 1 KiB');
  assert.equal(noteText(over), 'Over the limit by 0.5 KiB');
  // the parts add up to `used` and are drawn against it
  assert.equal(
    over.segments.reduce((n, s) => n + s.amount, 0),
    1500,
  );
  assert.equal(widthPct(over, 1500), 100);
  assert.ok(Math.abs(widthPct(over, 1000) - (100 * 2) / 3) < 1e-9);
});

test('the largest free block is named only when it is smaller than the free total', () => {
  const whole = by(buildBars(dn2, loaded, library), 'ARENA');
  assert.equal(whole.largest, whole.free);
  // the arena's largest is a payload: one free block of the whole free size reports 16 bytes less and is not split
  const oneBlock = by(buildBars({ ARENA: { limit: 1000, used: 400, largest: 584 } }, [], library), 'ARENA');
  assert.equal(noteText(oneBlock), null);
  // the DSP area's largest is a raw gap: no allowance
  const dspGap = by(buildBars({ DSP_AREA: { limit: 1000, used: 400, largest: 590 } }, [], library), 'DSP_AREA');
  assert.equal(dspGap.split, true);
  assert.equal(freeTitle(whole), `Free: ${kibText(whole.free)}`);
  assert.equal(noteText(whole), null);
  const split = by(
    buildBars({ ...dn2, ARENA: { limit: 512 * KiB, used: 12 * KiB, largest: 100 * KiB } }, loaded, library),
    'ARENA',
  );
  assert.equal(freeTitle(split), 'Free: 500 KiB, largest block 100 KiB');
  assert.equal(noteText(split), 'Free: 500 KiB, largest block 100 KiB');
  // a largest above the free total (a runtime rounding) is capped
  const capped = by(buildBars({ ARENA: { limit: 1000, used: 900, largest: 5000 } }, [], library), 'ARENA');
  assert.equal(capped.largest, 100);
});

test('the texts: KiB for bytes, counts for rows, the module name and amount on hover', () => {
  const bars = buildBars(dn2, loaded, library);
  const dsp = by(bars, 'DSP_AREA');
  const cpu = by(bars, 'ARENA');
  const mods = by(bars, 'MODULES');
  assert.equal(usedText(dsp), '259 KiB / 512 KiB');
  assert.equal(usedText(cpu), '13 KiB / 512 KiB');
  assert.equal(usedText(mods), '3 / 8');
  assert.equal(segmentTitle(dsp, dsp.segments[0]), 'sine: 259 KiB');
  assert.equal(segmentTitle(mods, mods.segments[1]), 'chorus: 1');
  assert.match(segmentTitle(cpu, cpu.segments[3]), /^Other: 6 KiB/);
  assert.equal(freeTitle(dsp), 'Free: 253 KiB');
  assert.deepEqual(
    [kibText(1032), kibText(4096), kibText(10 * KiB + 100), kibText(0)],
    ['1 KiB', '4 KiB', '10 KiB', '0 KiB'],
  );
  assert.equal(kibText(1500), '1.5 KiB');
  assert.equal(amountText({ bytes: false }, 7), '7');
  assert.equal(widthPct(dsp, 256 * KiB), 50);
  assert.equal(widthPct({ limit: 0 } as Bar, 5), 0);
});
