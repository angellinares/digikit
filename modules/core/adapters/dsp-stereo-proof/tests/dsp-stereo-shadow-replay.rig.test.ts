/**
 * Assembled synthetic DN2 subset; no claim about the stock default route. Port of test_dsp_stereo_shadow_replay.py (T1g) on the SHARC-only
 * rig with the stock DSP image: the right channel runs as a shadow of the left (its own state copy, the same stock stages) and must match
 * an isolated mono run of the same channel on a separate core. Store watchpoints bound the writes of each call, including the nested stock
 * frames, and count the writes per state word.
 *
 * RECIPS and RSQRTS use the measured seed tables of the core (finding 102), so the divisions of 1c06ba are exact. The separate filter leaves
 * use byte-space pointers for their stock 32-bit DM access context. No instruction semantics are modified. Original firmware is supplied at
 * run time, never copied into a fixture. Reference channels execute independently on separate cores with private state.
 *
 * Read bounds: the Python test also logs every DM read and checks the table loads at their PCs (address range and count). The rig records
 * stores only, so these checks are not ported (docs/plans/ledgers/t1g-retirement.md, "Shadow replay").
 */
import { describe, expect, it } from 'vitest';
import { DN2_112, stockAvailable, stockSkipReason } from '@digi/sharc-rig-kit';
import {
  BACK,
  CONTROL,
  FIRST,
  LEFT,
  REGS,
  RIGHT,
  STACK,
  UY,
  Shadow,
  bits,
  buildShadow,
  canonical,
  executed,
  toByte,
  type ShadowBuild,
} from './shadow-rig.ts';

const STOCK_BASE = { _control: 0x1cdb56, _first: 0xb80c39, _uy: 0xb81368 };
const enabled = stockAvailable(DN2_112);
const suite = enabled ? describe : describe.skip;
const title = 'the synthetic shadow replay of the stereo stages (DN2 1.12)';

const range = (n: number, from = 0): number[] => Array.from({ length: n }, (_, i) => from + i);
const stackWords = (): number[] => range(65, STACK - 64);

/** every store must be 4 bytes at an allowed word (`allowed`: normal-word addresses); the counts are per allowed word */
function checkStores(s: Shadow, allowed: Iterable<number>, label = 'stores', widths: Record<number, number> = {}): Map<number, number> {
  const byByte = new Map<number, number>();
  for (const a of allowed) byByte.set(canonical(toByte(a)), a);
  const outside = s.stores.filter(([, address, width]) => !byByte.has(address) || width !== (widths[byByte.get(address)!] ?? 4));
  if (outside.length)
    throw new Error(`${label}: ${outside.length} stores outside: ${outside.slice(0, 4).map(([pc, address, width]) => `${pc.toString(16)} ${address.toString(16)} w${width}`).join(', ')}`);
  const counts = new Map<number, number>();
  for (const [, address] of s.stores) {
    const a = byByte.get(address)!;
    counts.set(a, (counts.get(a) ?? 0) + 1);
  }
  return counts;
}
const count = (counts: Map<number, number>, a: number): number => counts.get(a) ?? 0;

async function open(build: ShadowBuild, n: number): Promise<Shadow[]> {
  return Promise.all(Array.from({ length: n }, () => Shadow.open(build)));
}
const closeAll = (sessions: Shadow[]): Promise<unknown> => Promise.all(sessions.map((s) => s.close()));
const words = (s: Shadow, base: number, n: number): Promise<number[]> => s.words(base, n);
const finite = (ws: number[]): boolean => ws.every((v) => (v & 0x7f800000) !== 0x7f800000);

// ---- the base object: controls, first-order and U/Y stages (`_prepare`, `_left`, `_right`, `_recurrences`) ----

const CALL_LIMIT = 12000;

async function call(s: Shadow, entry: string): Promise<void> {
  const r = await s.call(entry, REGS, { maxSteps: CALL_LIMIT });
  expect(r.halt, `${entry}: ${JSON.stringify({ ...r, stores: r.stores.length })}`).toBe('breakpoint');
  expect(r.final_pc).toBe(BACK);
  // Canonical byte-address watchpoints bound every executed store, including nested stock frames. Preparation may copy controls and
  // coefficients only.
  let allowed: Set<number>;
  if (entry === '_prepare') {
    allowed = new Set([...range(14).map((i) => CONTROL + 0x100 + i), ...range(3, 21).map((i) => FIRST + 0x100 + i), ...range(4, 2).map((i) => UY + 0x100 + i)]);
  } else {
    const offset = entry === '_right' || entry === '_right_recurrences' ? 0x100 : 0;
    allowed = new Set([
      ...range(32).map((i) => LEFT + offset + i),
      ...[4, 5, 6, 11, 12, 13].map((i) => CONTROL + offset + i),
      ...[24, 25].map((i) => FIRST + offset + i),
      ...[6, 7].map((i) => UY + offset + i),
      ...range(65, STACK - 64),
    ]);
  }
  const counts = checkStores(s, allowed);
  if (entry === '_left' || entry === '_right') {
    const offset = entry === '_right' ? 0x100 : 0;
    expect(range(32).map((i) => count(counts, LEFT + offset + i))).toEqual(Array(32).fill(4));
    expect([4, 5, 6, 11, 12, 13].map((i) => count(counts, CONTROL + offset + i))).toEqual(Array(6).fill(2));
  }
}

async function initialize(s: Shadow): Promise<void> {
  for (const [base, n] of [[CONTROL, 14], [FIRST, 28], [UY, 8]] as const) for (let i = 0; i < n; i++) s.poke(base + i, 0);
  for (const [i, v] of Object.entries({ 0: 0.8, 1: 0.3, 2: 0.6, 5: 0.2, 12: 0.1 })) s.poke(CONTROL + Number(i), bits(v));
  for (const [i, v] of Object.entries({ 21: 0.7, 22: 0.2, 23: 0.1 })) s.poke(FIRST + Number(i), bits(v));
  for (const [i, v] of Object.entries({ 2: 0.6, 3: 0.3, 4: 0.2, 5: 0.4 })) s.poke(UY + Number(i), bits(v));
}

/** the private right histories start at zero; preparation must retain them */
function zeroRightHistories(s: Shadow): void {
  for (const [base, indices] of [[FIRST + 0x100, [24, 25]], [UY + 0x100, [6, 7]]] as const) for (const i of indices) s.poke(base + i, 0);
}

suite(enabled ? title : `${title}: ${stockSkipReason(DN2_112)}`, () => {
  const base: ShadowBuild = enabled ? buildShadow('base', ['stereo-shadow-replay.s'], 0xb90000, STOCK_BASE) : (undefined as never);

  it('multiblock: the shadow matches isolated mono, and a reset reproduces the first block', async () => {
    const [stereo, left, right] = await open(base, 3);
    const sessions = [stereo!, left!, right!];
    try {
      for (const s of sessions) await initialize(s);
      zeroRightHistories(stereo!);
      let firstSnapshot: unknown;
      for (const [iteration, block] of [0, 1, 2, 0].entries()) {
        if (iteration === 3) {
          // Explicit reset of BOTH audio histories and shared controls. The same block must reproduce the first execution exactly.
          for (const s of sessions) await initialize(s);
          zeroRightHistories(stereo!);
        }
        for (const s of sessions) {
          s.poke(CONTROL, bits(0.8 - 0.1 * block));
          s.poke(FIRST + 21, bits(0.7 - 0.05 * block));
          s.poke(UY + 2, bits(0.6 + 0.02 * block));
        }
        for (let i = 0; i < 32; i++) {
          const lv = bits((i - 11) / 64 + block / 32);
          const rv = bits((7 - i) / 83 - block / 64);
          stereo!.poke(LEFT + i, lv);
          stereo!.poke(RIGHT + i, rv);
          left!.poke(LEFT + i, lv);
          right!.poke(LEFT + i, rv);
        }
        await call(stereo!, '_prepare');
        await call(stereo!, '_left');
        const authoritative = await words(stereo!, CONTROL, 14);
        await call(stereo!, '_right');
        await call(left!, '_left');
        await call(right!, '_left');
        expect(await words(stereo!, LEFT, 32)).toEqual(await words(left!, LEFT, 32));
        expect(await words(stereo!, RIGHT, 32)).toEqual(await words(right!, LEFT, 32));
        expect(await words(stereo!, CONTROL, 14)).toEqual(authoritative);
        expect(authoritative).toEqual(await words(left!, CONTROL, 14));
        expect(await words(stereo!, CONTROL + 0x100, 14)).toEqual(await words(right!, CONTROL, 14));
        expect(await words(stereo!, LEFT, 32)).not.toEqual(await words(stereo!, RIGHT, 32));
        for (const b of [FIRST, UY]) {
          // Ignore unaudited padding: only coefficients and histories are used.
          const indices = b === FIRST ? range(5, 21) : range(6, 2);
          for (const [source, reference, offset] of [[stereo!, left!, 0], [stereo!, right!, 0x100]] as const)
            expect(await Promise.all(indices.map((i) => source.peek(b + offset + i)))).toEqual(
              await Promise.all(indices.map((i) => reference.peek(b + i))),
            );
        }
        const snapshot = [
          await words(stereo!, LEFT, 32),
          await words(stereo!, RIGHT, 32),
          await words(stereo!, CONTROL, 14),
          await words(stereo!, CONTROL + 0x100, 14),
          await Promise.all(
            [[FIRST, [24, 25]], [UY, [6, 7]]].flatMap(([b, indices]) =>
              [0, 0x100].flatMap((offset) => (indices as number[]).map((i) => stereo!.peek((b as number) + offset + i))),
            ),
          ),
        ];
        if (iteration === 0) firstSnapshot = snapshot;
        else if (iteration === 3) expect(snapshot).toEqual(firstSnapshot);
      }
    } finally {
      await closeAll(sessions);
    }
  });

  it('recurrences: the default core matches independent channels (no RECIPS)', async () => {
    const [stereo, left, right] = await open(base, 3);
    const sessions = [stereo!, left!, right!];
    try {
      for (const s of sessions) await initialize(s);
      zeroRightHistories(stereo!);
      for (let block = 0; block < 3; block++) {
        for (const s of sessions) {
          s.poke(FIRST + 21, bits(0.7 - block / 32));
          s.poke(UY + 2, bits(0.6 + block / 64));
        }
        for (let i = 0; i < 32; i++) {
          const lv = bits((i - 13) / 64 + block / 32);
          const rv = bits((11 - i) / 89);
          stereo!.poke(LEFT + i, lv);
          stereo!.poke(RIGHT + i, rv);
          left!.poke(LEFT + i, lv);
          right!.poke(LEFT + i, rv);
        }
        await call(stereo!, '_prepare');
        await call(stereo!, '_recurrences');
        await call(stereo!, '_right_recurrences');
        await call(left!, '_recurrences');
        await call(right!, '_recurrences');
        for (const [reference, output, offset] of [[left!, LEFT, 0], [right!, RIGHT, 0x100]] as const) {
          expect(await words(stereo!, output, 32)).toEqual(await words(reference, LEFT, 32));
          for (const [b, indices] of [[FIRST, [24, 25]], [UY, [6, 7]]] as const)
            expect(await Promise.all(indices.map((i) => stereo!.peek(b + offset + i)))).toEqual(
              await Promise.all(indices.map((i) => reference.peek(b + i))),
            );
        }
      }
    } finally {
      await closeAll(sessions);
    }
  });

  it.each(['copy-advanced-controls', 'reset-right-history', 'reset-controls-between-occurrences'])(
    'fault injection is observable: %s',
    async (fault) => {
      const [candidate, reference] = await open(base, 2);
      try {
        for (const s of [candidate!, reference!]) {
          await initialize(s);
          zeroRightHistories(s);
        }
        const differences: boolean[] = [];
        let preControls: number[] = [];
        for (let block = 0; block < 2; block++) {
          for (const s of [candidate!, reference!]) {
            s.poke(CONTROL, bits(0.9 - block / 8));
            for (let i = 0; i < 32; i++) {
              s.poke(LEFT + i, bits((i - 10) / 64));
              s.poke(RIGHT + i, bits((5 - i) / 73));
            }
            await call(s, '_prepare');
            if (s === candidate) preControls = await words(s, CONTROL + 0x100, 14);
            await call(s, '_left');
          }
          if (fault === 'copy-advanced-controls') {
            // The shadow must start from the pre-left snapshot, not its end.
            await call(candidate!, '_prepare');
          } else if (fault === 'reset-right-history') {
            zeroRightHistories(candidate!);
          }
          await call(reference!, '_right');
          if (fault === 'reset-controls-between-occurrences') {
            // Pause after U/Y, before the repeated control occurrence. Reusing the preblock snapshot here loses the first occurrence's writes.
            const boundary = candidate!.pc('_ru');
            let result = await candidate!.call('_right', REGS, { maxSteps: CALL_LIMIT, breaks: [BACK, boundary] });
            expect(result.final_pc).toBe(boundary);
            preControls.forEach((w, i) => candidate!.poke(CONTROL + 0x100 + i, w));
            result = await candidate!.call('_right', REGS, { maxSteps: CALL_LIMIT, resume: true });
            expect(result.halt).toBe('breakpoint');
            expect(result.final_pc).toBe(BACK);
          } else {
            await call(candidate!, '_right');
          }
          const actual = [await words(candidate!, RIGHT, 32), await words(candidate!, CONTROL, 14)];
          const expected = [await words(reference!, RIGHT, 32), await words(reference!, CONTROL, 14)];
          differences.push(JSON.stringify(actual) !== JSON.stringify(expected));
        }
        expect(differences.some(Boolean)).toBe(true);
      } finally {
        await closeAll([candidate!, reference!]);
      }
    },
  );

});

// ---- the filter leaves and the prepared stages ----

// These are isolated finite leaves, not dispatch routing or the case-5 blend. snapshots = b82342, nine = b81a16, six = b81526. Small
// coefficients and nonzero per-sample increments exercise their inspected float recurrences without exceptional values; they are synthetic
// captured coefficients.
const FILTER_FAMILIES: Record<string, { size: number; histories: number[]; coefficients: number[]; values: number[] }> = {
  snapshots: {
    size: 51,
    histories: [3, 4, 48, 49, 50],
    coefficients: [5, 6, 7, 12, 14, 26, 28, 33, 35, 40, 42],
    values: [0.2, 0.3, 0.1, 0.4, 0.0002, 0.2, 0.0001, 0.1, 0.0001, 0.1, 0.0001],
  },
  nine: {
    size: 75,
    histories: [10, 11],
    coefficients: range(9).flatMap((k) => [12 + 7 * k, 14 + 7 * k]),
    values: [0.1, 0.2, 0.1, 0.1, 0.05, 0.05, 0.1, 0.1, 0.1].flatMap((v) => [v, 0.0001]),
  },
  six: {
    size: 53,
    histories: [9, 10],
    coefficients: range(6).flatMap((k) => [11 + 7 * k, 13 + 7 * k]),
    values: [0.6, 0.1, 0.15, 0.2, 0.1, 0.08].flatMap((v) => [v, 0.0001]),
  },
};

const FILTER_STOCK = {
  _snapshots: 0xb82342,
  _nine: 0xb81a16,
  _six: 0xb81526,
  _held: 0xb8265b,
  _prepared: 0xb80f2e,
};
const FILTER_FILES = ['stereo-filter-shadow-replay.s', 'stereo-prepared-shadow-replay.s'];
const PREPARED_WRITES = range(6).flatMap((k) => [0, 1, 2].map((delta) => 17 + 7 * k + delta));
const PREPARED_SHARED = [0, 1, 2, 3, 4, 14, ...PREPARED_WRITES];
const BYTE_REGS = { ...REGS, I6: STACK * 4, I7: STACK * 4 };
const WIDE_REGS = { ...BYTE_REGS, M13: 0, M15: 0xffffffff };

/** the audio each leaf reads at the table loads (Python watches the loads; the rig counts how often the load instructions run) */
const TABLE_LOADS: Record<string, { pcs: number[]; executions: number }> = {
  held: { pcs: [0xb826e0], executions: 1 },
  prepared: { pcs: [0xb81053, 0xb81059], executions: 64 },
};

async function filterCall(s: Shadow, family: string, side: string, histories: number[], coefficients: number[], sharedWrites: number[] = []): Promise<void> {
  const loads = TABLE_LOADS[family];
  const r = await s.call(`_${side}_${family}`, BYTE_REGS, { maxSteps: CALL_LIMIT, profile: side !== 'prepare' && loads !== undefined });
  expect(r.halt, `${side} ${family}: ${JSON.stringify({ ...r, stores: r.stores.length })}`).toBe('breakpoint');
  expect(r.final_pc).toBe(BACK);
  if (side === 'prepare') {
    const allowed = coefficients.map((i) => CONTROL + 0x100 + i);
    const counts = checkStores(s, allowed);
    expect([...counts.keys()].sort()).toEqual([...allowed].sort());
    expect(new Set(counts.values())).toEqual(new Set([1]));
    return;
  }
  const offset = side === 'right' ? 0x100 : 0;
  const allowed = [...range(32).map((i) => LEFT + offset + i), ...[...histories, ...sharedWrites].map((i) => CONTROL + offset + i), ...stackWords()];
  const counts = checkStores(s, allowed);
  expect(range(32).map((i) => count(counts, LEFT + offset + i))).toEqual(Array(32).fill(1));
  expect(histories.every((i) => count(counts, CONTROL + offset + i) > 0)).toBe(true);
  expect(sharedWrites.every((i) => count(counts, CONTROL + offset + i) === 1)).toBe(true);
  if (loads) expect(executed(r, loads.pcs), `table loads of ${family}`).toBe(loads.executions);
}

suite(enabled ? `${title}: filter leaves` : `${title}: filter leaves: ${stockSkipReason(DN2_112)}`, () => {
  const filter: ShadowBuild = enabled ? buildShadow('filter', FILTER_FILES, 0xb92000, FILTER_STOCK) : (undefined as never);

  it.each(Object.keys(FILTER_FAMILIES).flatMap((family) => [false, true].map((corrupt) => [family, corrupt] as const)))(
    'filter leaf: the shadow matches isolated mono: %s, corrupt history %s',
    async (family, corruptHistory) => {
      const { size, histories, coefficients, values } = FILTER_FAMILIES[family]!;
      const [stereo, left, right] = await open(filter, 3);
      const sessions = [stereo!, left!, right!];
      try {
        for (const s of sessions) for (const offset of [0, 0x100]) for (let i = 0; i < size; i++) s.poke(CONTROL + offset + i, 0);
        // Unequal nonzero initial histories also make block zero sensitive to aliasing.
        histories.forEach((i, n) => {
          const lv = bits((n + 1) / 64);
          const rv = bits(-(n + 1) / 89);
          stereo!.poke(CONTROL + i, lv);
          left!.poke(CONTROL + i, lv);
          stereo!.poke(CONTROL + 0x100 + i, rv);
          right!.poke(CONTROL + i, rv);
        });
        const differences: boolean[] = [];
        const audioDifferences: boolean[] = [];
        for (let block = 0; block < 3; block++) {
          for (const s of sessions) coefficients.forEach((i, k) => s.poke(CONTROL + i, bits(values[k]! * (1 + block / 16))));
          const inputs: [number, number][] = [];
          for (let i = 0; i < 32; i++) {
            const lv = bits((i - 11) / 64 + block / 32);
            const rv = bits((7 - i) / 83 - block / 64);
            inputs.push([lv, rv]);
            stereo!.poke(LEFT + i, lv);
            stereo!.poke(RIGHT + i, rv);
            left!.poke(LEFT + i, lv);
            right!.poke(LEFT + i, rv);
          }
          const beforeRight = await Promise.all(histories.map((i) => stereo!.peek(CONTROL + 0x100 + i)));
          await filterCall(stereo!, family, 'prepare', histories, coefficients);
          expect(await Promise.all(histories.map((i) => stereo!.peek(CONTROL + 0x100 + i)))).toEqual(beforeRight);
          await filterCall(stereo!, family, 'left', histories, coefficients);
          const authoritative = await words(stereo!, CONTROL, size);
          if (corruptHistory) {
            // Deliberately alias all audited histories/snapshots to the left end state.
            for (const i of histories) stereo!.poke(CONTROL + 0x100 + i, (await stereo!.peek(CONTROL + i)) as number);
          }
          await filterCall(stereo!, family, 'right', histories, coefficients);
          await filterCall(left!, family, 'left', histories, coefficients);
          await filterCall(right!, family, 'left', histories, coefficients);
          expect(await words(stereo!, CONTROL, size)).toEqual(authoritative);
          expect(authoritative).toEqual(await words(left!, CONTROL, size));
          expect(await words(stereo!, LEFT, 32)).toEqual(await words(left!, LEFT, 32));
          expect(await words(left!, LEFT, 32)).not.toEqual(inputs.map((v) => v[0]));
          expect(await words(right!, LEFT, 32)).not.toEqual(inputs.map((v) => v[1]));
          expect(await words(stereo!, LEFT, 32)).not.toEqual(await words(stereo!, RIGHT, 32));
          const actual = [await words(stereo!, RIGHT, 32), await words(stereo!, CONTROL + 0x100, size)];
          const expected = [await words(right!, LEFT, 32), await words(right!, CONTROL, size)];
          differences.push(JSON.stringify(actual) !== JSON.stringify(expected));
          audioDifferences.push(JSON.stringify(actual[0]) !== JSON.stringify(expected[0]));
          expect((await Promise.all(histories.map((i) => stereo!.peek(CONTROL + i)))).some((v) => v !== 0)).toBe(true);
          if (!corruptHistory) expect(actual).toEqual(expected);
          for (const s of sessions) for (const output of s === stereo ? [LEFT, RIGHT] : [LEFT]) expect(finite(await words(s, output, 32))).toBe(true);
        }
        if (corruptHistory) {
          expect(differences.some(Boolean)).toBe(true);
          expect(audioDifferences.some(Boolean)).toBe(true);
        }
      } finally {
        await closeAll(sessions);
      }
    },
  );

  async function initializedPrepared(family: string): Promise<{ sessions: Shadow[]; histories: number[]; size: number }> {
    const sessions = await open(filter, 3);
    const histories = family === 'held' ? [1, 2] : [5, 9, 10, 11, 12];
    const size = family === 'held' ? 5 : 59;
    for (const s of sessions) {
      for (const offset of [0, 0x100]) for (let i = 0; i < size; i++) s.poke(CONTROL + offset + i, 0);
      if (family === 'held') {
        // b82440 resets counters to M13. A finite in-period entry of 1 exposes private held values before the next refresh.
        for (const i of [3, 4]) s.poke(CONTROL + i, 1);
      } else {
        // These are captured finite controls, not a live setter invocation. b811f1 seeds the six current coefficients from these expressions.
        const controls: Record<number, number> = { 0: 0.1, 1: 0.3, 2: 0.08, 3: 0.1, 4: 0.6, 14: 0.2 };
        const current = [controls[1]!, controls[0]!, controls[2]! / 4, controls[3]!, controls[4]!, -controls[14]! / 8];
        for (const [i, v] of Object.entries(controls)) s.poke(CONTROL + Number(i), bits(v));
        current.forEach((v, k) => {
          for (const delta of [0, 1]) s.poke(CONTROL + 17 + 7 * k + delta, bits(v));
        });
      }
    }
    const [stereo, left, right] = sessions;
    histories.forEach((i, n) => {
      const lv = bits((n + 1) / 256);
      const rv = bits(-(n + 1) / 311);
      stereo!.poke(CONTROL + i, lv);
      left!.poke(CONTROL + i, lv);
      stereo!.poke(CONTROL + 0x100 + i, rv);
      right!.poke(CONTROL + i, rv);
    });
    return { sessions, histories, size };
  }

  it.each(['held', 'prepared'].flatMap((family) => [null, 'advanced-controls', 'shared-history'].map((fault) => [family, fault] as const)))(
    'prepared stage: the shadow matches isolated mono: %s, fault %s',
    async (family, fault) => {
      const { sessions, histories, size } = await initializedPrepared(family);
      const [stereo, left, right] = sessions as [Shadow, Shadow, Shadow];
      try {
        const shared = family === 'held' ? [0, 3, 4] : PREPARED_SHARED;
        const writes = family === 'held' ? [3, 4] : PREPARED_WRITES;
        const audioDifferences: boolean[] = [];
        let previous = await Promise.all(histories.map((i) => stereo.peek(CONTROL + i)));
        for (let block = 0; block < 3; block++) {
          for (const s of sessions) {
            if (family === 'held') {
              // Active clamp/index path: trunc(127*c) is 32,48,64. The original table periods are finite and between 6 and 26; each block
              // therefore both holds and refreshes samples.
              s.poke(CONTROL, bits((32 + 16 * block) / 127));
            } else {
              // b81024 clips before table indexing. The lookup coordinate is 1023*(0.5 + ramp(-control14/8)*clipped), with |clipped|<=1.
              // Initial/current control14 stays below 0.25, so both adjacent indices remain strictly inside the 1024-word original table.
              for (const [i, v] of Object.entries({ 0: 0.12, 1: 0.32, 2: 0.1, 3: 0.12, 4: 0.65, 14: 0.22 })) s.poke(CONTROL + Number(i), bits(v * (1 + block / 16)));
            }
          }
          const inputs: [number, number][] = [];
          for (let i = 0; i < 32; i++) {
            const lv = bits((i - 11) / 128 + block / 64);
            const rv = bits((7 - i) / 163 - block / 128);
            inputs.push([lv, rv]);
            stereo.poke(LEFT + i, lv);
            stereo.poke(RIGHT + i, rv);
            left.poke(LEFT + i, lv);
            right.poke(LEFT + i, rv);
          }
          const rightHistory = await Promise.all(histories.map((i) => stereo.peek(CONTROL + 0x100 + i)));
          await filterCall(stereo, family, 'prepare', histories, shared);
          expect(await Promise.all(histories.map((i) => stereo.peek(CONTROL + 0x100 + i)))).toEqual(rightHistory);
          await filterCall(stereo, family, 'left', histories, shared, writes);
          const authoritative = await words(stereo, CONTROL, size);
          if (fault === 'advanced-controls') await filterCall(stereo, family, 'prepare', histories, shared);
          else if (fault === 'shared-history') for (const i of histories) stereo.poke(CONTROL + 0x100 + i, (await stereo.peek(CONTROL + i)) as number);
          await filterCall(stereo, family, 'right', histories, shared, writes);
          await filterCall(left, family, 'left', histories, shared, writes);
          await filterCall(right, family, 'left', histories, shared, writes);
          expect(await words(stereo, CONTROL, size)).toEqual(authoritative);
          expect(authoritative).toEqual(await words(left, CONTROL, size));
          expect(await words(stereo, LEFT, 32)).toEqual(await words(left, LEFT, 32));
          const actual = await words(stereo, RIGHT, 32);
          const expected = await words(right, LEFT, 32);
          audioDifferences.push(JSON.stringify(actual) !== JSON.stringify(expected));
          if (fault === null) {
            expect(actual).toEqual(expected);
            expect(await words(stereo, CONTROL + 0x100, size)).toEqual(await words(right, CONTROL, size));
            expect(await Promise.all(shared.map((i) => stereo.peek(CONTROL + i)))).toEqual(await Promise.all(shared.map((i) => stereo.peek(CONTROL + 0x100 + i))));
          }
          expect(await words(stereo, LEFT, 32)).not.toEqual(inputs.map((v) => v[0]));
          expect(expected).not.toEqual(inputs.map((v) => v[1]));
          if (family === 'held') {
            // Every block includes held output and at least one refresh, not merely the disabled copy path or a block with stale held values.
            for (const output of [await words(stereo, LEFT, 32), expected]) {
              expect(output.slice(0, -1).some((a, k) => a === output[k + 1])).toBe(true);
              expect(new Set(output).size).toBeGreaterThan(1);
            }
          }
          expect(await words(stereo, LEFT, 32)).not.toEqual(actual);
          const current = await Promise.all(histories.map((i) => stereo.peek(CONTROL + i)));
          expect(current).not.toEqual(previous);
          previous = current;
          for (const s of sessions) for (const output of s === stereo ? [LEFT, RIGHT] : [LEFT]) expect(finite(await words(s, output, 32))).toBe(true);
        }
        if (fault !== null) expect(audioDifferences.some(Boolean)).toBe(true);
      } finally {
        await closeAll(sessions);
      }
    },
  );
});

// ---- the gain stage (b80345) ----

const gainBuild = (): ShadowBuild => buildShadow('gain', ['stereo-gain-shadow-replay.s'], 0xb94000, { _gain: 0xb80345 });
/** the five table loads of the gain stage (Python watches the loads and checks their range; the rig tells which of the load instructions ran) */
const GAIN_TABLE_PCS = [0xb8039c, 0xb803d7, 0xb80452, 0xb8049d, 0xb804fc];

async function gainCall(s: Shadow, side: string, control = 0): Promise<number[]> {
  const r = await s.call(`_${side}_gain`, { ...WIDE_REGS, R0: control, R1: 0 }, { maxSteps: CALL_LIMIT, profile: true });
  expect(r.halt, `${side} gain: ${JSON.stringify({ ...r, stores: r.stores.length })}`).toBe('breakpoint');
  expect(r.final_pc).toBe(BACK);
  if (side === 'prepare') {
    const allowed = range(11).map((i) => CONTROL + 0x100 + i);
    const counts = checkStores(s, allowed);
    expect([...counts.keys()].sort()).toEqual([...allowed].sort());
    expect(new Set(counts.values())).toEqual(new Set([1]));
    return [];
  }
  const offset = side === 'right' ? 0x100 : 0;
  const allowed = [...[7, 9, 10].map((i) => CONTROL + offset + i), ...range(32).map((i) => LEFT + offset + i), ...stackWords()];
  const counts = checkStores(s, allowed);
  // Unity/hold paths may leave in-place audio untouched. Each stored sample must still occur once and remain inside the 32-word plane.
  expect(range(32).every((i) => count(counts, LEFT + offset + i) <= 1)).toBe(true);
  return GAIN_TABLE_PCS.filter((pc) => executed(r, [pc]) > 0);
}

suite(enabled ? `${title}: gain` : `${title}: gain: ${stockSkipReason(DN2_112)}`, () => {
  const build = enabled ? gainBuild() : (undefined as never);

  it.each([false, true].flatMap((advanced) => [false, true].map((hold) => [advanced, hold] as const)))(
    'gain: the shadow matches isolated mono: advanced controls %s, hold-release %s',
    async (advancedControls, holdRelease) => {
      const sessions = await open(build, 3);
      const [stereo, left, right] = sessions as [Shadow, Shadow, Shadow];
      try {
        // b8030c/b80338 initialize words 8, 7, 10, but do not initialize 9. Captured b8028c setter arguments yield words 0..6 below; word 9 is
        // explicitly zeroed here. Reinitialization below is the b80345 entry control boundary only, not a claim about a named trigger or reset
        // event. Setter capture: R8=0, R12=1/16, stack(1/64,1/16,1/2,1/16,1). The optional finite variant enables the inspected 14-sample hold
        // then phase 4 release to zero; it does not invoke an unaudited live setter.
        const initial = [7, 1, 7, bits(holdRelease ? 0 : 0.5), 7, 1, holdRelease ? 1 : 0, 5, 0, 0, 0];
        for (const s of sessions) for (const offset of [0, 0x100]) initial.forEach((v, i) => s.poke(CONTROL + offset + i, v));
        const phases: number[] = [];
        const markers: number[] = [];
        const tablePcs = new Set<number>();
        const audioDifferences: boolean[] = [];
        for (const [block, control] of [1, 0, 0, 0, 0, 1, 0, 0, 0].entries()) {
          const inputs: [number, number][] = [];
          for (let i = 0; i < 32; i++) {
            const lv = bits((i - 11) / 64 + block / 64);
            const rv = bits((7 - i) / 83 - block / 128);
            inputs.push([lv, rv]);
            stereo.poke(LEFT + i, lv);
            stereo.poke(RIGHT + i, rv);
            left.poke(LEFT + i, lv);
            right.poke(LEFT + i, rv);
          }
          await gainCall(stereo, 'prepare');
          const snapshot = await words(stereo, CONTROL, 11);
          expect(await words(stereo, CONTROL + 0x100, 11)).toEqual(snapshot);
          for (const pc of await gainCall(stereo, 'left', control)) tablePcs.add(pc);
          const authoritative = await words(stereo, CONTROL, 11);
          if (advancedControls) await gainCall(stereo, 'prepare');
          await gainCall(stereo, 'right', control);
          await gainCall(left, 'left', control);
          await gainCall(right, 'left', control);
          expect(await words(stereo, CONTROL, 11)).toEqual(authoritative);
          expect(authoritative).toEqual(await words(left, CONTROL, 11));
          expect(await words(left, CONTROL, 11)).toEqual(await words(right, CONTROL, 11));
          expect(await words(stereo, LEFT, 32)).toEqual(await words(left, LEFT, 32));
          const actual = await words(stereo, RIGHT, 32);
          const expected = await words(right, LEFT, 32);
          audioDifferences.push(JSON.stringify(actual) !== JSON.stringify(expected));
          if (!advancedControls) {
            expect(actual).toEqual(expected);
            expect(await words(stereo, CONTROL + 0x100, 11)).toEqual(authoritative);
          }
          expect(await words(stereo, LEFT, 32)).not.toEqual(inputs.map((v) => v[0]));
          expect(expected).not.toEqual(inputs.map((v) => v[1]));
          if (holdRelease && (block === 2 || block === 7)) {
            // The bounded phase 3 segment is unity with in-place source/dest. Retain its input even if no multiply/store is needed there.
            const originalRight = inputs.map((v) => v[1]);
            expect(range(19).some((i) => JSON.stringify(expected.slice(i, i + 14)) === JSON.stringify(originalRight.slice(i, i + 14)))).toBe(true);
          }
          expect(await words(stereo, LEFT, 32)).not.toEqual(actual);
          phases.push((await stereo.peek(CONTROL + 7)) as number);
          markers.push((await stereo.peek(CONTROL + 10)) as number);
          for (const s of sessions) for (const output of s === stereo ? [LEFT, RIGHT] : [LEFT]) expect(finite(await words(s, output, 32))).toBe(true);
        }
        // The first attack crosses the -1 clamp in block 2, then reaches phase 1 or the finite hold/phase 4 path. Entry control 1 later restarts
        // the attack.
        const expectedPhases = [2, 2, holdRelease ? 4 : 1];
        expect(phases.slice(0, 3)).toEqual(expectedPhases);
        expect(phases.slice(5, 8)).toEqual(expectedPhases);
        for (const pc of [0xb8039c, 0xb803d7]) expect(tablePcs.has(pc), pc.toString(16)).toBe(true);
        for (const pc of holdRelease ? [0xb80452, 0xb804fc] : [0xb8049d]) expect(tablePcs.has(pc), pc.toString(16)).toBe(true);
        expect(markers[0]).not.toBe(markers[1]);
        expect(markers[1]).not.toBe(markers[2]);
        expect(markers.slice(5, 8)).toEqual(markers.slice(0, 3));
        if (advancedControls) expect(audioDifferences.some(Boolean)).toBe(true);
      } finally {
        await closeAll(sessions);
      }
    },
  );
});

// ---- the ring stage ----

const RING_LEFT = 0x10040000;
const RING_RIGHT = 0x10042000;
const RING_WORDS = 8192;
const RING_PRIVATE = [0, 13, 14, 16, 17];
const RING_SHARED = range(18).filter((i) => !RING_PRIVATE.includes(i));
/** the four indexed loads of the ring read 32 samples each (Python: 32 * 4 loads of 4 bytes) */
const RING_LOAD_PCS = [0xb8079b, 0xb8079e, 0xb807a4, 0xb807a7];

async function ringCall(s: Shadow, side: string, initialization = false): Promise<void> {
  const entry = initialization ? `_init_${side}_ring` : `_${side}_ring`;
  const r = await s.call(entry, WIDE_REGS, { maxSteps: CALL_LIMIT, profile: true });
  expect(r.halt, `${entry}: ${JSON.stringify({ ...r, stores: r.stores.length })}`).toBe('breakpoint');
  expect(r.final_pc).toBe(BACK);
  const offset = side === 'right' ? 0x100 : 0;
  if (side === 'prepare') {
    const allowed = RING_SHARED.map((i) => CONTROL + 0x100 + i);
    const counts = checkStores(s, allowed);
    expect([...counts.keys()].sort()).toEqual([...allowed].sort());
    expect(new Set(counts.values())).toEqual(new Set([1]));
    return;
  }
  const mutable = initialization ? range(18).filter((i) => ![13, 15, 16].includes(i)) : [0, 3, 4, 5, 13, 14, 17];
  const ring = side === 'right' ? RING_RIGHT : RING_LEFT;
  const allowed = [
    ...mutable.map((i) => CONTROL + offset + i),
    ...stackWords(),
    ...(initialization ? [] : range(32).map((i) => LEFT + offset + i)),
    ...range(RING_WORDS).map((i) => ring + i),
  ];
  // the fill word is stored as one byte
  const counts = (() => {
    try {
      return checkStores(s, allowed, 'ring store bounds', { [CONTROL + offset + 17]: 1 });
    } catch (error) {
      throw new Error(`ring store bounds: ${(error as Error).message}`);
    }
  })();
  if (initialization) {
    expect(range(RING_WORDS).map((i) => count(counts, ring + i))).toEqual(Array(RING_WORDS).fill(1));
  } else {
    expect(range(32).map((i) => count(counts, LEFT + offset + i))).toEqual(Array(32).fill(1));
    expect(range(RING_WORDS).reduce((n, i) => n + count(counts, ring + i), 0)).toBe(32);
    // the actual indexed loads ran 32 times each (their addresses are checked against this channel's backing by the Python test only)
    expect(executed(r, RING_LOAD_PCS)).toBe(32 * 4);
  }
}

async function initializedRing(build: ShadowBuild, stereo = false): Promise<Shadow> {
  const s = await Shadow.open(build);
  for (const [side, offset, ring] of [['left', 0, RING_LEFT], ['right', 0x100, RING_RIGHT]] as const) {
    if (side === 'right' && !stereo) break;
    for (let i = 0; i < 18; i++) s.poke(CONTROL + offset + i, 0);
    // Constructor obtains this byte pointer from word 16. Its actual SIMD clear and 48000-Hz coefficient initialization run on private memory.
    s.poke(CONTROL + offset + 16, toByte(ring));
    await ringCall(s, side, true);
    expect(await words(s, ring, RING_WORDS)).toEqual(Array(RING_WORDS).fill(0));
  }
  return s;
}

suite(enabled ? `${title}: ring` : `${title}: ring: ${stockSkipReason(DN2_112)}`, () => {
  const build = enabled ? buildShadow('ring', ['stereo-ring-shadow-replay.s'], 0xb96000, { _ring: 0xb806f5, _ring_init: 0xb80575 }) : (undefined as never);

  it.each([false, true])('ring: the shadow matches isolated mono, shared pointer %s', async (sharedPointer) => {
    const sessions: Shadow[] = [];
    try {
      const stereo = await initializedRing(build, true);
      sessions.push(stereo);
      const left = await initializedRing(build);
      sessions.push(left);
      const right = await initializedRing(build);
      sessions.push(right);
      for (const [s, offset, channel] of [[stereo, 0, 0], [stereo, 0x100, 1], [left, 0, 0], [right, 0, 1]] as const) {
        const ring = offset ? RING_RIGHT : RING_LEFT;
        s.raw.pokeWords(toByte(ring), range(RING_WORDS).map((i) => bits(((i % 97) - 48) / (512 + 97 * channel))));
        // Audited setter clamps delay to 1..8191; exact integer32 avoids the float wrap-to-8192 endpoint. Coordinates model a filled near-wrap ring.
        for (const [i, v] of Object.entries({ 0: bits((1 - 2 * channel) / 32), 13: bits(8158), 14: 8190, 15: bits(32), 17: 0 })) s.poke(CONTROL + offset + Number(i), v);
      }
      const differences: boolean[] = [];
      for (let block = 0; block < 3; block++) {
        for (const s of [stereo, left, right]) s.poke(CONTROL + 10, bits(0.1 + block / 32));
        for (let i = 0; i < 32; i++) {
          const lv = bits((i - 11) / 128 + block / 64);
          const rv = bits((7 - i) / 163 - block / 128);
          stereo.poke(LEFT + i, lv);
          stereo.poke(RIGHT + i, rv);
          left.poke(LEFT + i, lv);
          right.poke(LEFT + i, rv);
        }
        const priv = await Promise.all(RING_PRIVATE.map((i) => stereo.peek(CONTROL + 0x100 + i)));
        await ringCall(stereo, 'prepare');
        expect(await Promise.all(RING_PRIVATE.map((i) => stereo.peek(CONTROL + 0x100 + i)))).toEqual(priv);
        await ringCall(stereo, 'left');
        const authoritative = await words(stereo, CONTROL, 18);
        const leftBacking = await words(stereo, RING_LEFT, RING_WORDS);
        if (sharedPointer) {
          stereo.poke(CONTROL + 0x100 + 16, toByte(RING_LEFT));
          await expect(ringCall(stereo, 'right')).rejects.toThrow('ring store bounds');
          expect(await words(stereo, RING_LEFT, RING_WORDS)).not.toEqual(leftBacking);
          return;
        }
        await ringCall(stereo, 'right');
        await ringCall(left, 'left');
        await ringCall(right, 'left');
        expect(await words(stereo, CONTROL, 18)).toEqual(authoritative);
        expect(authoritative).toEqual(await words(left, CONTROL, 18));
        expect(await words(stereo, RING_LEFT, RING_WORDS)).toEqual(leftBacking);
        expect(leftBacking).toEqual(await words(left, RING_LEFT, RING_WORDS));
        expect(await words(stereo, RING_RIGHT, RING_WORDS)).toEqual(await words(right, RING_LEFT, RING_WORDS));
        expect(await words(stereo, LEFT, 32)).toEqual(await words(left, LEFT, 32));
        expect(await words(stereo, RIGHT, 32)).toEqual(await words(right, LEFT, 32));
        expect(await words(stereo, LEFT, 32)).not.toEqual(await words(stereo, RIGHT, 32));
        // Reference mono has an independently owned backing at another pointer.
        for (let i = 0; i < 18; i++) if (i !== 16) expect(await stereo.peek(CONTROL + 0x100 + i), `word ${i}`).toBe(await right.peek(CONTROL + i));
        expect(await stereo.peek(CONTROL + 14)).toBe((8190 + 32 * (block + 1)) % RING_WORDS);
        expect(await stereo.peek(CONTROL + 0x100 + 14)).toBe(await stereo.peek(CONTROL + 14));
        expect(await stereo.peek(CONTROL + 13)).toBe(bits((8158 + 32 * (block + 1)) % RING_WORDS));
        expect(await stereo.peek(CONTROL + 0x100 + 13)).toBe(await stereo.peek(CONTROL + 13));
        expect(await stereo.peek(CONTROL)).not.toBe(bits(1 / 32));
        for (const s of sessions) for (const output of s === stereo ? [LEFT, RIGHT] : [LEFT]) expect(finite(await words(s, output, 32))).toBe(true);
        differences.push(JSON.stringify(await words(stereo, LEFT, 32)) !== JSON.stringify(await words(stereo, RIGHT, 32)));
      }
      expect(differences.every(Boolean)).toBe(true);
    } finally {
      await closeAll(sessions);
    }
  });
});

// ---- the source continuation (b8251b) ----

/** the five instructions that load the audio samples as SIMD double-word loads: 16 runs read the 128 bytes of 32 samples (Python: 32 * 4 byte loads) */
const SOURCE_SAMPLE_PCS = [0xb8255f, 0xb8256e, 0xb825e3, 0xb825e6, 0xb82613];
const SOURCE_TABLE_PCS = [0xb825af, 0xb825b3];
const SOURCE_SCRATCH = { S0: 0x12340000, S5: 0x12340005, S9: 0x12340009, S14: 0x1234000e };

async function sourceCall(s: Shadow, side: string, depth: number): Promise<void> {
  // Companion scratch is explicitly seeded, not treated as callee-preserved.
  const regs = { ...BYTE_REGS, ...SOURCE_SCRATCH };
  const r = await s.call(`_${side}_source`, regs, { maxSteps: CALL_LIMIT, readRegs: ['MODE1', 'S0', 'S5', 'S9', 'S14'], profile: true });
  expect(r.halt, `${side} source: ${JSON.stringify({ ...r, stores: r.stores.length })}`).toBe('breakpoint');
  expect(r.final_pc).toBe(BACK);
  expect(r.regs.MODE1).toBe(0);
  const offset = side === 'right' ? 0x100 : 0;
  const counts = checkStores(s, [...range(32).map((i) => LEFT + offset + i), ...stackWords()]);
  expect(range(32).map((i) => count(counts, LEFT + offset + i))).toEqual(Array(32).fill(1));
  // The loads: the 32 samples once (16 double-word loads), and two table words (two loads) when the depth is nonzero.
  expect(executed(r, SOURCE_SAMPLE_PCS)).toBe(16);
  expect(executed(r, SOURCE_TABLE_PCS)).toBe(depth ? 2 : 0);
  if (depth) {
    expect(r.regs.S9).toBe((-depth) >>> 0);
    expect(r.regs.S14).toBe((-depth - 1) >>> 0);
  } else {
    for (const name of ['S0', 'S5', 'S9', 'S14'] as const) expect(r.regs[name], name).toBe(SOURCE_SCRATCH[name]);
  }
}

suite(enabled ? `${title}: source` : `${title}: source: ${stockSkipReason(DN2_112)}`, () => {
  const build = enabled ? buildShadow('source', ['stereo-source-shadow-replay.s'], 0xb98000, { _source: 0xb8251b }) : (undefined as never);

  it.each([[0, 0], [0.25, 8], [0.5, 4], [0.75, 2], [1, 1]] as const)('source continuation matches isolated mono: control %f, depth %i', async (control, depth) => {
    const sessions = await open(build, 3);
    const [stereo, left, right] = sessions as [Shadow, Shadow, Shadow];
    try {
      for (const s of sessions) for (const offset of [0, 0x100]) s.poke(CONTROL + offset, bits(control));
      const inputs: [number, number][] = [];
      for (let i = 0; i < 32; i++) {
        const lv = bits((i - 15) / 21);
        const rv = bits((11 - i) / 27);
        inputs.push([lv, rv]);
        stereo.poke(LEFT + i, lv);
        stereo.poke(RIGHT + i, rv);
        left.poke(LEFT + i, lv);
        right.poke(LEFT + i, rv);
      }
      await sourceCall(stereo, 'left', depth);
      const authoritative = await stereo.peek(CONTROL);
      await sourceCall(stereo, 'right', depth);
      await sourceCall(left, 'left', depth);
      await sourceCall(right, 'left', depth);
      expect(await stereo.peek(CONTROL)).toBe(authoritative);
      expect(authoritative).toBe(bits(control));
      for (const s of sessions) {
        expect(await s.peek(CONTROL)).toBe(await s.peek(CONTROL + 0x100));
        expect(await s.peek(CONTROL)).toBe(bits(control));
      }
      for (const [output, reference, channel] of [[LEFT, left, 0], [RIGHT, right, 1]] as const) {
        const result = await words(stereo, output, 32);
        expect(result).toEqual(await words(reference, LEFT, 32));
        const original = inputs.map((v) => v[channel]);
        if (depth) expect(result).not.toEqual(original);
        else expect(result).toEqual(original);
        expect(finite(result)).toBe(true);
      }
      expect(await words(stereo, LEFT, 32)).not.toEqual(await words(stereo, RIGHT, 32));
    } finally {
      await closeAll(sessions);
    }
  });
});

// ---- the ordered route: source, first-order and case-5 prefix, the original blend, gain and the original pan ----

const ORDER_ENGINE = 0x08040600;
const ORDER_GAIN = 0x08040800;
const ORDER_PAN = 0x08040a00;
const ORDER_INPUTS = 0x08040c00;
const ORDER_OUTPUTS = 0x08040e00;
const ORDER_SCRATCH = 0x08041200;
const ORDER_PLANES = [0x08041400, 0x08041500, 0x08041600, 0x08041700];
const ORDER_SHARED: [number, number[]][] = [
  [CONTROL, [0]],
  [FIRST, [21, 22, 23]],
  [UY, range(6).flatMap((k) => [11 + 7 * k, 13 + 7 * k])],
  [ORDER_ENGINE, [0, 2, 7, 9]],
  [ORDER_GAIN, range(11)],
  [ORDER_PAN, [0, 2, 112, 114]],
];
const ORIGINAL_BLEND = 0x1c9c12;
const ORIGINAL_BLEND_END = 0x1c98b9;
const ORIGINAL_PAN = 0x1c9aea;
const ORIGINAL_PAN_END = 0x1c9b30;

type Phase = 'prepare' | 'prefix' | 'blend' | 'gain' | 'pan';

async function orderedCall(s: Shadow, side: string, phase: Phase, control = 0): Promise<void> {
  const offset = side === 'right' ? 0x100 : 0;
  const audio = LEFT + offset;
  const scratch = ORDER_SCRATCH + offset;
  const engine = ORDER_ENGINE + offset;
  const gain = ORDER_GAIN + offset;
  const pan = ORDER_PAN + offset;
  const histories = [...[24, 25].map((i) => FIRST + i), ...[9, 10].map((i) => UY + i)];
  const shared = ORDER_SHARED.flatMap(([base, indices]) => indices.map((i) => base + i));
  const regs: Record<string, number> = { ...WIDE_REGS };
  let entry: string | number;
  let stop = BACK;
  let writes: number[];
  if (phase === 'prepare') {
    entry = '_prepare_ordered';
    writes = shared.map((a) => a + 0x100);
  } else if (phase === 'prefix') {
    entry = `_${side}_ordered`;
    writes = [...range(32).map((i) => audio + i), ...range(32).map((i) => scratch + i), ...histories.map((a) => a + offset), ...stackWords()];
  } else if (phase === 'blend') {
    entry = ORIGINAL_BLEND;
    stop = ORIGINAL_BLEND_END;
    // Original continuation reloads the scratch pointer from engine+4e02.
    Object.assign(regs, { I1: (CONTROL + offset) * 4, I0: engine * 4, I5: audio * 4, R9: 32 });
    writes = range(32).map((i) => audio + i);
  } else if (phase === 'gain') {
    entry = `_${side}_ordered_gain`;
    Object.assign(regs, { R0: control, R1: 0 });
    writes = [...range(32).map((i) => audio + i), ...[7, 9, 10].map((i) => gain + i), ...stackWords()];
  } else {
    entry = ORIGINAL_PAN;
    stop = ORIGINAL_PAN_END;
    Object.assign(regs, { I3: (ORDER_INPUTS + offset) * 4, I5: (ORDER_OUTPUTS + offset) * 4, I4: pan * 4, R9: 32 });
    const planes = offset ? ORDER_PLANES.slice(2) : ORDER_PLANES.slice(0, 2);
    writes = planes.flatMap((base) => range(32).map((i) => base + i));
  }
  const r = await s.call(entry, regs, { maxSteps: CALL_LIMIT, breaks: [BACK, stop], readRegs: ['MODE1'] });
  expect(r.halt, `${side} ${phase}: ${JSON.stringify({ ...r, stores: r.stores.length })}`).toBe('breakpoint');
  expect(r.final_pc).toBe(stop);
  expect(r.regs.MODE1).toBe(0);
  const counts = checkStores(s, writes, `${side} ${phase}`);
  if (phase === 'prepare' || phase === 'pan') {
    expect([...counts.keys()].sort()).toEqual([...new Set(writes)].sort());
    expect(new Set(counts.values())).toEqual(new Set([1]));
  } else if (phase === 'prefix') {
    expect(range(32).map((i) => count(counts, audio + i))).toEqual(Array(32).fill(2));
    expect(range(32).map((i) => count(counts, scratch + i))).toEqual(Array(32).fill(1));
  } else if (phase === 'blend') {
    expect(range(32).map((i) => count(counts, audio + i))).toEqual(Array(32).fill(1));
  }
}

async function initializedOrdered(build: ShadowBuild, channel = 0): Promise<Shadow> {
  const s = await Shadow.open(build);
  for (const offset of [0, 0x100]) {
    for (const [base, n] of [[CONTROL, 1], [FIRST, 28], [UY, 53], [ORDER_ENGINE, 10], [ORDER_GAIN, 11], [ORDER_PAN, 115]] as const)
      for (let i = 0; i < n; i++) s.poke(base + offset + i, 0);
    [7, 1, 7, bits(0.5), 7, 1, 0, 5, 0, 0, 0].forEach((v, i) => s.poke(ORDER_GAIN + offset + i, v));
    const sideChannel = offset ? 1 : channel;
    for (const [base, indices, values] of [
      [FIRST, [24, 25], [[0.03, -0.01], [-0.02, 0.04]]],
      [UY, [9, 10], [[0.02, -0.03], [-0.04, 0.01]]],
    ] as const)
      indices.forEach((i, k) => s.poke(base + offset + i, bits(values[sideChannel]![k]!)));
    s.poke(CONTROL + offset + 0x4e02, (ORDER_SCRATCH + offset) * 4);
    s.poke(ORDER_INPUTS + offset, (LEFT + offset) * 4);
    const planes = offset ? ORDER_PLANES.slice(2) : ORDER_PLANES.slice(0, 2);
    planes.forEach((plane, i) => s.poke(ORDER_OUTPUTS + offset + i, plane * 4));
  }
  return s;
}

interface Channel {
  prefix: number[];
  scratch: number[];
  blend: number[];
  gain: number[];
  planes: number[][];
}

async function runOrderedChannel(s: Shadow, side: string, control: number, overwriteScratch = false): Promise<Channel> {
  const offset = side === 'right' ? 0x100 : 0;
  await orderedCall(s, side, 'prefix');
  const prefix = await words(s, LEFT + offset, 32);
  const scratch = await words(s, ORDER_SCRATCH + offset, 32);
  if (overwriteScratch) {
    // Deliberate lifetime fault after the case5 result but before its original continuation consumes it. Bounds remain valid.
    for (let i = 0; i < 32; i++) s.poke(ORDER_SCRATCH + offset + i, 0);
  }
  await orderedCall(s, side, 'blend');
  const blend = await words(s, LEFT + offset, 32);
  await orderedCall(s, side, 'gain', control);
  const gain = await words(s, LEFT + offset, 32);
  await orderedCall(s, side, 'pan');
  const planes = offset ? ORDER_PLANES.slice(2) : ORDER_PLANES.slice(0, 2);
  return { prefix, scratch, blend, gain, planes: await Promise.all(planes.map((plane) => words(s, plane, 32))) };
}

suite(enabled ? `${title}: ordered route` : `${title}: ordered route: ${stockSkipReason(DN2_112)}`, () => {
  const build = enabled
    ? buildShadow('ordered', ['stereo-ordered-shadow-replay.s'], 0xb9a000, { _source: 0xb8251b, _first: 0xb80c39, _case5: 0xb81526, _gain: 0xb80345 })
    : (undefined as never);

  // Selected synthetic route: scalar/later ramps unity, other conditionals and fade off. Shared cleanup is outside replay and is not proven
  // here. The terminal L.A/R.B pair is a candidate balance mapping, not an ABI, stock-default reachability, or mixed/send consumer
  // integration proof.
  it.each([null, 'shared-history', 'advanced-gain', 'scratch-overwrite', 'swapped-terminal', 'duplicated-terminal'])(
    'ordered route matches isolated mono: fault %s',
    async (fault) => {
      const stereo = await initializedOrdered(build);
      const left = await initializedOrdered(build);
      const right = await initializedOrdered(build, 1);
      const sessions = [stereo, left, right];
      try {
        const differences: boolean[] = [];
        const phases: number[] = [];
        for (let block = 0; block < 4; block++) {
          for (const s of sessions) {
            s.poke(CONTROL, bits(0.5));
            for (const [i, v] of Object.entries({ 21: 0.7 - block / 64, 22: 0.2, 23: 0.1 })) s.poke(FIRST + Number(i), bits(v));
            [0.6, 0.1, 0.15, 0.2, 0.1, 0.08].forEach((v, k) => {
              s.poke(UY + 11 + 7 * k, bits(v * (1 + block / 32)));
              s.poke(UY + 13 + 7 * k, bits(0.0001));
            });
            for (const [i, v] of Object.entries({ 0: 0.4 + block / 64, 2: 0.001, 7: 0.6 - block / 64, 9: -0.001 })) s.poke(ORDER_ENGINE + Number(i), bits(v));
            for (const [i, v] of Object.entries({ 0: 0.25 + block / 32, 2: 0.002, 112: 0.8 - block / 32, 114: -0.001 })) s.poke(ORDER_PAN + Number(i), bits(v));
          }
          for (let i = 0; i < 32; i++) {
            const lv = bits((i - 15) / 21 + block / 64);
            const rv = bits((11 - i) / 27 - block / 128);
            stereo.poke(LEFT + i, lv);
            stereo.poke(RIGHT + i, rv);
            left.poke(LEFT + i, lv);
            right.poke(LEFT + i, rv);
          }
          await orderedCall(stereo, 'left', 'prepare');
          const actualLeft = await runOrderedChannel(stereo, 'left', Number(block === 0));
          const watched = [...ORDER_SHARED, [FIRST, [24, 25]] as [number, number[]], [UY, [9, 10]] as [number, number[]]].flatMap(([base, indices]) => indices.map((i) => base + i));
          const authoritative = await Promise.all(watched.map((a) => stereo.peek(a)));
          const leftScratch = await words(stereo, ORDER_SCRATCH, 32);
          if (fault === 'shared-history') {
            for (const [base, indices] of [[FIRST, [24, 25]], [UY, [9, 10]]] as const)
              for (const i of indices) stereo.poke(base + 0x100 + i, (await stereo.peek(base + i)) as number);
          } else if (fault === 'advanced-gain') {
            await orderedCall(stereo, 'left', 'prepare');
          }
          const actualRight = await runOrderedChannel(stereo, 'right', Number(block === 0), fault === 'scratch-overwrite');
          const expectedLeft = await runOrderedChannel(left, 'left', Number(block === 0));
          const expectedRight = await runOrderedChannel(right, 'left', Number(block === 0));
          expect(actualLeft).toEqual(expectedLeft);
          expect(await Promise.all(watched.map((a) => stereo.peek(a)))).toEqual(authoritative);
          expect(await words(stereo, ORDER_SCRATCH, 32)).toEqual(leftScratch);
          if (fault === 'scratch-overwrite') {
            expect(actualRight.prefix).toEqual(expectedRight.prefix);
            expect(actualRight.scratch).toEqual(expectedRight.scratch);
            expect(actualRight.blend).not.toEqual(expectedRight.blend);
          }
          if (fault === null || fault === 'swapped-terminal' || fault === 'duplicated-terminal') {
            expect(actualRight).toEqual(expectedRight);
            expect(await words(stereo, ORDER_GAIN + 0x100, 11)).toEqual(await words(stereo, ORDER_GAIN, 11));
          }
          let terminal = [actualLeft.planes[0], actualRight.planes[1]];
          if (fault === 'swapped-terminal') terminal = [actualLeft.planes[1]!, actualRight.planes[0]!];
          else if (fault === 'duplicated-terminal') terminal = [actualLeft.planes[0]!, actualLeft.planes[0]!];
          const expected = [expectedLeft.planes[0], expectedRight.planes[1]];
          differences.push(JSON.stringify(terminal) !== JSON.stringify(expected));
          if (fault === null) expect(terminal).toEqual(expected);
          expect(actualLeft.planes[0]).not.toEqual(actualRight.planes[1]);
          for (const snapshots of [actualLeft, actualRight]) {
            expect(snapshots.prefix).not.toEqual(snapshots.scratch);
            expect(snapshots.blend).not.toEqual(snapshots.prefix);
            expect(snapshots.gain).not.toEqual(snapshots.blend);
            for (const plane of [...snapshots.planes, snapshots.prefix, snapshots.scratch, snapshots.blend, snapshots.gain]) expect(finite(plane)).toBe(true);
          }
          phases.push((await left.peek(ORDER_GAIN + 7)) as number);
        }
        expect(phases).toEqual([2, 2, 1, 1]);
        if (fault !== null) expect(differences.some(Boolean)).toBe(true);
      } finally {
        await closeAll(sessions);
      }
    },
  );
});

// ---- the helper resets: each reset helper runs on the left record, then mirrors on the right record ----

const RESET_WRITES: Record<string, number[]> = {
  snapshots: [3, 4, 12, 13, 14, 17, 19, 20, 21, 24, 26, 27, 28, 31, 33, 34, 35, 38, 40, 41, 42, 45],
  nine: [10, 11, ...range(9).flatMap((k) => [0, 1, 2, 5].map((delta) => 12 + 7 * k + delta))],
  prepared: [...range(8, 5), ...PREPARED_WRITES, ...range(6).map((k) => 22 + 7 * k)],
  held: range(5),
};
const RESET_SIZES: Record<string, number> = { snapshots: 51, nine: 75, prepared: 59, held: 5 };
const RESET_HISTORIES: Record<string, number[]> = { snapshots: [3, 4, 48, 49, 50], nine: [10, 11], prepared: [5, 9, 10, 11, 12], held: [1, 2] };
const RESET_STOCK = { _reset_snapshots: 0xb81d6d, _reset_nine: 0xb81c8d, _reset_prepared: 0xb811f1, _reset_held: 0xb82440 };

async function resetCall(s: Shadow, family: string, side: string): Promise<void> {
  const r = await s.call(`_reset_${side}_${family}`, WIDE_REGS, { maxSteps: CALL_LIMIT });
  expect(r.halt, `${side} reset ${family}: ${JSON.stringify({ ...r, stores: r.stores.length })}`).toBe('breakpoint');
  expect(r.final_pc).toBe(BACK);
  const base = CONTROL + (side === 'right' ? 0x100 : 0);
  const mutable = RESET_WRITES[family]!.map((i) => base + i);
  const counts = checkStores(s, [...mutable, ...stackWords()], `${side} reset ${family}`);
  // every mutable word is written
  expect(mutable.filter((a) => !counts.has(a))).toEqual([]);
}

suite(enabled ? `${title}: helper resets` : `${title}: helper resets: ${stockSkipReason(DN2_112)}`, () => {
  const resetBuild = (): ShadowBuild => {
    const filter = buildShadow('filter', FILTER_FILES, 0xb92000, FILTER_STOCK);
    return buildShadow('reset', ['stereo-reset-shadow-replay.s'], 0xb9c000, RESET_STOCK, [filter]);
  };
  const build = enabled ? resetBuild() : (undefined as never);

  async function initializedReset(family: string, channel = 0): Promise<Shadow> {
    const s = await Shadow.open(build);
    const size = RESET_SIZES[family]!;
    for (const offset of [0, 0x100]) {
      for (let i = 0; i < size; i++) s.poke(CONTROL + offset + i, bits(0.01 + i / 512));
      let controls: Record<number, number>;
      if (family === 'snapshots') {
        const { coefficients, values } = FILTER_FAMILIES.snapshots!;
        controls = Object.fromEntries(coefficients.map((i, k) => [i, values[k]!]));
        Object.assign(controls, { 9: 0.2, 10: 0.25, 11: 0.3, 16: 0.4, 23: 0.1, 30: 0.15, 37: 0.2, 44: 0.25 });
      } else if (family === 'nine') {
        controls = Object.fromEntries([0.1, 0.2, 0.1, 0.1, 0.05, 0.05, 0.1, 0.1, 0.1].map((v, k) => [k + 1, v]));
      } else if (family === 'prepared') {
        controls = { 0: 0.1, 1: 0.3, 2: 0.08, 3: 0.1, 4: 0.6, 14: 0.2 };
      } else {
        controls = { 0: 0.25 };
        s.poke(CONTROL + offset + 3, 1);
        s.poke(CONTROL + offset + 4, 1);
      }
      for (const [i, v] of Object.entries(controls)) s.poke(CONTROL + offset + Number(i), bits(v));
      const sideChannel = offset ? 1 : channel;
      RESET_HISTORIES[family]!.forEach((i, n) => s.poke(CONTROL + offset + i, bits((n + 1) / (sideChannel === 0 ? 64 : -89))));
    }
    return s;
  }

  // Helper boundaries only: no event-name, global writer or reset-hook claim. b82440 is the held-sample record reset here, not an assumption
  // that every use of that helper has the same record layout as the source continuation.
  it.each([
    ...['snapshots', 'nine', 'prepared', 'held'].flatMap((family) => [null, 'missed-reset'].map((fault) => [family, fault] as const)),
    ['snapshots', 'blanket-snapshots'] as const,
  ])('helper reset mirroring matches isolated mono: %s, fault %s', async (family, fault) => {
    const stereo = await initializedReset(family);
    const left = await initializedReset(family);
    const right = await initializedReset(family, 1);
    const sessions = [stereo, left, right];
    try {
      const size = RESET_SIZES[family]!;
      const beforeRight = await words(stereo, CONTROL + 0x100, size);
      await resetCall(stereo, family, 'left');
      const authoritative = await words(stereo, CONTROL, size);
      await resetCall(left, family, 'left');
      await resetCall(right, family, 'left');
      if (fault !== 'missed-reset') await resetCall(stereo, family, 'right');
      if (fault === 'blanket-snapshots') {
        // b81d6d resets 3/4, but retains 48..50. Their external consumers are unaudited, so check loss exactly at the reset boundary; the leaf
        // renderer later overwrites these snapshots and can hide the error.
        for (const i of [48, 49, 50]) stereo.poke(CONTROL + 0x100 + i, 0);
      }
      expect(await words(stereo, CONTROL, size)).toEqual(authoritative);
      expect(authoritative).toEqual(await words(left, CONTROL, size));
      const actual = await words(stereo, CONTROL + 0x100, size);
      const expected = await words(right, CONTROL, size);
      if (fault === null) expect(actual).toEqual(expected);
      else expect(actual).not.toEqual(expected);
      if (family === 'snapshots') {
        expect(await Promise.all([48, 49, 50].map((i) => right.peek(CONTROL + i)))).toEqual([48, 49, 50].map((i) => beforeRight[i]));
        expect(await Promise.all([3, 4].map((i) => right.peek(CONTROL + i)))).toEqual([0, 0]);
        expect([48, 49, 50].every((i) => beforeRight[i] !== 0)).toBe(true);
      }
      // Run each actual renderer after resetting.
      if (family === 'held') {
        // Capture an active post-reset control, retaining actual reset counters and held values. A missed reset must not be hidden by a copy
        // bypass.
        for (const [s, offset] of [[stereo, 0], [stereo, 0x100], [left, 0], [right, 0]] as const) s.poke(CONTROL + offset, bits(0.25));
      }
      for (let i = 0; i < 32; i++) {
        const lv = bits((i - 11) / 128);
        const rv = bits((7 - i) / 163);
        stereo.poke(LEFT + i, lv);
        stereo.poke(RIGHT + i, rv);
        left.poke(LEFT + i, lv);
        right.poke(LEFT + i, rv);
      }
      let histories: number[];
      let shared: number[];
      let sharedWrites: number[];
      if (family in FILTER_FAMILIES) {
        ({ histories, coefficients: shared } = FILTER_FAMILIES[family]!);
        sharedWrites = [];
      } else {
        histories = RESET_HISTORIES[family]!;
        shared = family === 'held' ? [0, 3, 4] : PREPARED_SHARED;
        sharedWrites = family === 'held' ? [3, 4] : PREPARED_WRITES;
      }
      await filterCall(stereo, family, 'left', histories, shared, sharedWrites);
      const authoritativeAfter = await words(stereo, CONTROL, size);
      await filterCall(stereo, family, 'right', histories, shared, sharedWrites);
      await filterCall(left, family, 'left', histories, shared, sharedWrites);
      await filterCall(right, family, 'left', histories, shared, sharedWrites);
      expect(await words(stereo, CONTROL, size)).toEqual(authoritativeAfter);
      expect(authoritativeAfter).toEqual(await words(left, CONTROL, size));
      expect(await words(stereo, LEFT, 32)).toEqual(await words(left, LEFT, 32));
      const actualAudio = await words(stereo, RIGHT, 32);
      const expectedAudio = await words(right, LEFT, 32);
      if (fault === 'missed-reset') expect(actualAudio).not.toEqual(expectedAudio);
      else expect(actualAudio).toEqual(expectedAudio);
      expect(await words(stereo, LEFT, 32)).not.toEqual(actualAudio);
      expect(expectedAudio.some((v) => (v & 0x7fffffff) !== 0)).toBe(true);
      for (const s of sessions) for (const output of s === stereo ? [LEFT, RIGHT] : [LEFT]) expect(finite(await words(s, output, 32))).toBe(true);
    } finally {
      await closeAll(sessions);
    }
  });
});
