import fs from 'node:fs';
import path from 'node:path';
import { repoRoot } from '@digi/test-support';
import { expect } from 'vitest';
import type { PatchedSession } from '../support.ts';
import { expectRuntimeReady, moduleArtifact, runtimeTests } from './live.ts';

/** The probe transport (with remote also selected): the SHARC probe effect sends one framed word per block through the module step word, and the ColdFire collector keeps it. */
const PROBE = 'test/sharc-probe';
const COLLECTOR = 'digikit/probe';
/** Selected too: it takes service command slots (the collector got E_PROTO on a device when remote used all of them). */
const REMOTE = 'digikit/remote';
const CMD_STATUS = 0x32;
const CMD_READ = 0x33;
const CMD_RESET = 0x34;
/** A run is 1674 frames and takes about 33 s of emulated time; the test checks this many frames from the start of the capture, not a whole run. */
const FRAMES = 60;

function status(body: Uint8Array): Record<string, number> {
  const keys = [
    'format',
    'blocks',
    'records',
    'drops',
    'repeats',
    'idle',
    'words',
    'overruns',
    'cap',
    'parsed',
    'frames_ok',
    'seq_errors',
    'cks_errors',
  ];
  const view = new DataView(body.buffer, body.byteOffset, body.byteLength);
  return Object.fromEntries(keys.map((key, i) => [key, view.getUint32(4 * i)]));
}

async function installProbe({ session, dk }: PatchedSession, effect: string): Promise<void> {
  await expectRuntimeReady(session);
  const paths: string[] = [];
  for (const id of [REMOTE, effect, COLLECTOR]) {
    const bytes = fs.readFileSync(moduleArtifact(session.options.device, id));
    const path = `modules/${id.split('/').at(-1)}.dkm`;
    await dk.put(path, bytes);
    expect(await dk.verifyFile(path, bytes), `${id} reads back from the card`).toBe(true);
    paths.push(path);
  }
  await dk.select(paths);
  await dk.restart();
  await dk.waitForBoot();
  await expect
    .poll(async () => (await dk.modules()).map((m) => m.name), { timeout: 30_000, interval: 500 })
    .toEqual(expect.arrayContaining([REMOTE, effect, COLLECTOR]));
}

async function expectProbeTransport(patched: PatchedSession): Promise<void> {
  const { session, dk } = patched;
  await installProbe(patched, PROBE);
  expect((await dk.request(CMD_RESET)).status).toBe(0);
  // the effect starts a few thousand blocks after the boot; the collector then fills its ring
  let last: Record<string, number> = {};
  await expect
    .poll(
      async () => {
        const reply = await dk.request(CMD_STATUS);
        expect(reply.status, 'STATUS answers').toBe(0);
        last = status(reply.body);
        return last.frames_ok;
      },
      { timeout: 300_000, interval: 2000, message: `${FRAMES} frames` },
    )
    .toBeGreaterThanOrEqual(FRAMES);
  expect(last.seq_errors, `sequence errors: ${JSON.stringify(last)}`).toBe(0);
  expect(last.cks_errors, `checksum errors: ${JSON.stringify(last)}`).toBe(0);
  expect(last.drops, `records the callback missed: ${JSON.stringify(last)}`).toBe(0);
  await session.expectNoFault('probe transport');
}

runtimeTests('the SHARC probe sends framed words that the collector receives without errors', expectProbeTransport, {
  timeout: 600_000,
});

/** The words the beacon stores for its call number n (the counter after the increment): the counter, the two initialised words and the descriptor magic, each held for two calls. */
const BEACON_K1 = 0x5a5a1234;
const BEACON_K2 = 0xc0de0001;
const BEACON_MAGIC = 0x444b4658;
function beaconWord(n: number): number {
  switch ((n >>> 1) & 3) {
    case 0:
      return (0xbe000000 | (n & 0xffffff)) >>> 0;
    case 1:
      return BEACON_K1;
    case 2:
      return BEACON_K2;
    default:
      return BEACON_MAGIC;
  }
}

async function readRing(dk: PatchedSession['dk'], count: number): Promise<number[]> {
  const words: number[] = [];
  while (words.length < count) {
    const body = new Uint8Array(6);
    new DataView(body.buffer).setUint32(0, words.length);
    new DataView(body.buffer).setUint16(4, 200);
    const reply = await dk.request(CMD_READ, body);
    expect(reply.status, 'READ answers').toBe(0);
    const view = new DataView(reply.body.buffer, reply.body.byteOffset, reply.body.byteLength);
    const got = (reply.body.length - 8) / 4;
    if (got === 0) break;
    for (let k = 0; k < got; k++) words.push(view.getUint32(8 + 4 * k));
  }
  return words;
}

/** The beacon stores one step word on every call. The block tail copies it into every reply record, so the collector gets a word from every block: no zeros, consecutive counter
 * values, and the initialised constants exact. */
async function expectBeaconWords(patched: PatchedSession): Promise<void> {
  const { session, dk } = patched;
  await installProbe(patched, 'test/step-beacon');
  expect((await dk.request(CMD_RESET)).status).toBe(0);
  const WORDS = 400;
  let last: Record<string, number> = {};
  await expect
    .poll(
      async () => {
        last = status((await dk.request(CMD_STATUS)).body);
        return last.words;
      },
      { timeout: 120_000, interval: 2000, message: 'beacon words' },
    )
    .toBeGreaterThanOrEqual(WORDS);
  expect(last.drops, `records the callback missed: ${JSON.stringify(last)}`).toBe(0);
  const words = await readRing(dk, WORDS);
  expect(words.length).toBe(WORDS);
  expect(words.filter((w) => w === 0).length, 'no zero words').toBe(0);
  // the first counter word names the call number; every word after it must follow the beacon's pattern for the next call
  const first = words.findIndex((w) => w >>> 24 === 0xbe);
  expect(first, 'a counter word in the ring').toBeGreaterThanOrEqual(0);
  const n0 = words[first]! & 0xffffff;
  const bad = words
    .slice(first)
    .map((w, i) => [i, w, beaconWord(n0 + i)] as const)
    .filter(([, w, want]) => w !== want);
  expect(bad.slice(0, 4), `words that break the pattern (call ${n0} at word ${first})`).toEqual([]);
  await session.expectNoFault('step beacon');
}

runtimeTests('the step beacon words reach the collector in every block', expectBeaconWords, { timeout: 300_000 });

/** The probe stream (tools/device/dkprobe_seq.py): frames of a header word (0xB0, payload size, frame number), 4 records of 3 words and a checksum. A run is a header record (run counter, item count, table checksum), a record of the start state and one record per item (result, ASTAT, STKY). */
const FRAME_MAGIC = 0xb0;
const RECORDS_PER_FRAME = 4;
const PAYLOAD_WORDS = 3 * RECORDS_PER_FRAME;

function checksum(words: number[]): number {
  let s1 = 0;
  let s2 = 0;
  for (const w of words) {
    s1 = (s1 + w) >>> 0;
    s2 = (s2 + s1) >>> 0;
  }
  return (s1 ^ s2) >>> 0;
}

/** Items by index from the words of a capture; a frame is placed in its run from the nearest earlier frame that starts with the run header (the first frames from the next one), as `dkprobe.build_table`. */
function probeItems(words: number[], itemCount: number): Map<number, [number, number, number]> {
  const frames: { num: number; payload: number[] }[] = [];
  for (let p = 0; p + PAYLOAD_WORDS + 1 < words.length;) {
    const h = words[p]!;
    if (h >>> 24 === FRAME_MAGIC && ((h >>> 16) & 0xff) === PAYLOAD_WORDS) {
      if (checksum(words.slice(p, p + PAYLOAD_WORDS + 1)) === words[p + PAYLOAD_WORDS + 1]) {
        frames.push({ num: h & 0xffff, payload: words.slice(p + 1, p + 1 + PAYLOAD_WORDS) });
        p += PAYLOAD_WORDS + 2;
        continue;
      }
    }
    p += 1;
  }
  const perRun = Math.ceil((2 + itemCount) / RECORDS_PER_FRAME);
  const anchors = frames.flatMap((f, i) => (f.payload[1] === itemCount && f.payload[0] !== 0xffffffff ? [i] : []));
  const items = new Map<number, [number, number, number]>();
  if (anchors.length === 0) return items;
  frames.forEach((f, i) => {
    const before = anchors.filter((a) => a <= i).at(-1);
    let pos: number;
    if (before !== undefined) pos = ((f.num - frames[before]!.num) & 0xffff) % perRun;
    else {
      const d = (frames[anchors[0]!]!.num - f.num) & 0xffff;
      pos = (perRun - (d % perRun)) % perRun;
    }
    for (let k = 0; k < RECORDS_PER_FRAME; k++) {
      const r = pos * RECORDS_PER_FRAME + k;
      if (r < 2 || r >= 2 + itemCount) continue;
      const w = f.payload.slice(3 * k, 3 * k + 3) as [number, number, number];
      items.set(r - 2, w);
    }
  });
  return items;
}

/** The ASTAT bits 0-13 and the STKY bits 0-9 that the instruction sets; the other bits are other state (stack status in STKY). */
const ASTAT_MASK = 0x3fff;
const STKY_MASK = 0x3ff;

/** RECIPS and RSQRTS in the emulator give the result word and the flags that the Digitakt II gave for every item of the probe (tests/fixtures/sharc/recips-rsqrts-dt2-1.17.json, measured on OS 1.17). */
async function expectSeedTables(patched: PatchedSession): Promise<void> {
  const { session, dk } = patched;
  const fixture = JSON.parse(
    fs.readFileSync(path.join(repoRoot(), 'tests/fixtures/sharc/recips-rsqrts-dt2-1.17.json'), 'utf8'),
  ) as { rows: string[] };
  await installProbe(patched, PROBE);
  expect((await dk.request(CMD_RESET)).status).toBe(0);
  let last: Record<string, number> = {};
  await expect
    .poll(
      async () => {
        last = status((await dk.request(CMD_STATUS)).body);
        return last.words! >= last.cap!;
      },
      { timeout: 300_000, interval: 2000, message: 'a full ring (one run and a margin)' },
    )
    .toBe(true);
  await expect
    .poll(
      async () => {
        last = status((await dk.request(CMD_STATUS)).body);
        return last.parsed! >= last.words! - (PAYLOAD_WORDS + 2);
      },
      { timeout: 60_000, interval: 1000 },
    )
    .toBe(true);
  expect(last.seq_errors, `sequence errors: ${JSON.stringify(last)}`).toBe(0);
  const words = await readRing(dk, last.words!);
  const items = probeItems(words, fixture.rows.length);
  expect(items.size, 'items found in the capture').toBe(fixture.rows.length);
  const bad: string[] = [];
  const kinds = new Map<string, number>();
  fixture.rows.forEach((row, i) => {
    const [op, input, result, astat, stky] = row.split(',');
    const got = items.get(i)!;
    const want = [parseInt(result!, 16), parseInt(astat!, 16), parseInt(stky!, 16)];
    const have = [got[0], got[1] & ASTAT_MASK, got[2] & STKY_MASK];
    const differs = ['result', 'ASTAT', 'STKY'].filter((_, k) => want[k] !== have[k]);
    if (differs.length === 0) return;
    kinds.set(differs.join('+'), (kinds.get(differs.join('+')) ?? 0) + 1);
    bad.push(
      `${i} ${op} ${input}: device ${want.map((w) => w.toString(16))} emulator ${have.map((w) => w.toString(16))}`,
    );
  });
  expect(
    bad.slice(0, 8),
    `${bad.length} of ${fixture.rows.length} items differ: ${JSON.stringify([...kinds])}`,
  ).toEqual([]);
  await session.expectNoFault('seed tables');
}

runtimeTests('RECIPS and RSQRTS give the measured result and flags for every probe item', expectSeedTables, {
  timeout: 600_000,
});

/** The value items of the dsp-facts probe (tests/fixtures/modules/dsp-facts) that the Digitakt II measured (tests/fixtures/sharc/dsp-facts-values-dt2-1.17.json): FIX, float ALU edge cases, fixed-point overflow with and without ALUSAT, the
 * float multiplier's invalid result, MR reads. A run of the stream is 37 frames (one frame per block). */
const FACTS = 'test/dsp-facts';
const FACTS_WORDS_PER_RUN = 37 * (PAYLOAD_WORDS + 2);

async function expectFactsValues(patched: PatchedSession): Promise<void> {
  const { session, dk } = patched;
  const fixture = JSON.parse(
    fs.readFileSync(path.join(repoRoot(), 'tests/fixtures/sharc/dsp-facts-values-dt2-1.17.json'), 'utf8'),
  ) as { items: number; rows: string[] };
  await installProbe(patched, FACTS);
  expect((await dk.request(CMD_RESET)).status).toBe(0);
  let last: Record<string, number> = {};
  const want = Math.min(3 * FACTS_WORDS_PER_RUN, 1200);
  await expect
    .poll(
      async () => {
        last = status((await dk.request(CMD_STATUS)).body);
        return last.words! >= want && last.parsed! >= last.words! - (PAYLOAD_WORDS + 2);
      },
      { timeout: 300_000, interval: 2000, message: 'three runs of the facts stream' },
    )
    .toBe(true);
  expect(last.seq_errors, `sequence errors: ${JSON.stringify(last)}`).toBe(0);
  const words = await readRing(dk, Math.min(last.words!, last.cap!));
  const items = probeItems(words, fixture.items);
  const bad: string[] = [];
  for (const row of fixture.rows) {
    const [index, op, input, result, astat, stky] = row.split(',');
    const got = items.get(parseInt(index!, 10));
    if (!got) {
      bad.push(`${index} ${op} ${input}: not in the capture`);
      continue;
    }
    const wantRow = [parseInt(result!, 16), parseInt(astat!, 16), parseInt(stky!, 16)];
    const have = [got[0], got[1] & ASTAT_MASK, got[2] & STKY_MASK];
    if (wantRow.some((w, k) => w !== have[k]))
      bad.push(
        `${index} ${op} ${input}: device ${wantRow.map((w) => w.toString(16))} emulator ${have.map((w) => w.toString(16))}`,
      );
  }
  expect(bad.slice(0, 8), `${bad.length} of ${fixture.rows.length} items differ`).toEqual([]);
  await session.expectNoFault('dsp facts');
}

runtimeTests(
  'the instruction facts of the Digitakt II hold for every value item of the dsp-facts probe',
  expectFactsValues,
  {
    timeout: 600_000,
  },
);
