import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { afterEach, describe, expect, test } from 'vitest';
import type { Device } from '@digi/test-support';
import { cardStore, uploadStore } from '../runtime/device-data.ts';
import { KIT_AREAS, overlayWrites, runtimeSafety, type SafetyPolicy } from '../runtime/write-safety.ts';

const directories: string[] = [];
afterEach(() => {
  for (const directory of directories.splice(0)) fs.rmSync(directory, { recursive: true, force: true });
});

/** A run directory with `count` numbered watch logs and summaries, and one overlay file. */
function run(sectors: number[], count = 1, summary: object = { events: 0, flush_hits: [], cpushl: 1 }) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'digi-write-safety-'));
  directories.push(dir);
  for (let i = 0; i < count; i++) {
    fs.writeFileSync(path.join(dir, `writewatch-${i}.jsonl`), '');
    fs.writeFileSync(path.join(dir, `writewatch-${i}.summary.json`), JSON.stringify(summary));
  }
  const overlay = path.join(dir, 'overlay.bin');
  fs.writeFileSync(
    overlay,
    Buffer.concat(
      sectors.map((sector) => {
        const record = Buffer.alloc(8 + 64 + 512);
        record.writeBigUInt64LE(BigInt(sector));
        return record;
      }),
    ),
  );
  return { output: path.join(dir, 'writewatch.jsonl'), overlay };
}

const check = (device: Device, sectors: number[], policy: SafetyPolicy = 'default', storeExtra: number[] = []) => {
  const { output, overlay } = run(sectors);
  return runtimeSafety(device, output, [overlay], 1, policy, storeExtra);
};

describe.each(['dt2', 'dn2'] as const)('runtime safety on %s', (device) => {
  const store = cardStore(device);
  const [kitLo, kitHi] = KIT_AREAS[device][0]!;

  test('passes with clean logs and allowed sectors', () => {
    const { output, overlay } = run([store.selA, store.selB, 0x58000], 2);
    const report = runtimeSafety(device, output, [overlay], 2);
    expect(report).toMatchObject({ pass: true, emulators: 2, cpushl: 2, write_events: 0, flush_hits: 0 });
  });

  test.each([
    ['write events', { events: 1, flush_hits: [], cpushl: 0 }],
    ['flush hits', { events: 0, flush_hits: [{ line: 16 }], cpushl: 0 }],
  ])('fails on %s', (_name, summary) => {
    const { output, overlay } = run([], 1, summary);
    expect(runtimeSafety(device, output, [overlay]).pass).toBe(false);
  });

  test('fails when fewer emulators than the minimum left a summary', () => {
    const { output, overlay } = run([], 1);
    expect(() => runtimeSafety(device, output, [overlay], 2)).toThrow('missing or incomplete');
  });

  test('default policy allows no kit area and no store sector', () => {
    expect(check(device, [kitLo]).overlays[0]!.invalid_low).toEqual([kitLo]);
    expect(check(device, [store.first]).overlays[0]!.invalid_store).toEqual([store.first]);
    expect(check(device, [7]).pass).toBe(false);
  });

  test('fx-kit allows exactly the kit area, without the settings copies', () => {
    expect(check(device, [kitLo, kitHi - 1], 'fx-kit').pass).toBe(true);
    for (const sector of [kitLo - 1, kitHi, store.first, store.setA]) {
      expect(check(device, [sector], 'fx-kit').pass, `sector ${sector}`).toBe(false);
    }
  });

  test('explicit scenario store sectors are reported and applied without widening firmware or adjacent ranges', () => {
    expect(check(device, [store.setA]).pass).toBe(false);
    const allowed = check(device, [store.setA], 'default', [store.setA]);
    expect(allowed.pass).toBe(true);
    expect(allowed.policy.store_extra).toEqual([store.setA]);
    expect(check(device, [store.setA, store.setA + 1], 'default', [store.setA]).pass).toBe(false);
    expect(check(device, [kitLo, store.setA], 'default', [store.setA]).pass).toBe(false);
    expect(check(device, [kitLo, store.setA], 'fx-kit', [store.setA]).pass).toBe(true);
  });

  test('fx-routes also allows the settings copies, and nothing else in the store', () => {
    expect(check(device, [kitLo, store.setA, store.setB], 'fx-routes').pass).toBe(true);
    for (const sector of [kitLo - 1, kitHi, store.first, store.setA + 1, store.setB + 1]) {
      expect(check(device, [sector], 'fx-routes').pass, `sector ${sector}`).toBe(false);
    }
  });

  test('outsideStore lists only sectors below the firmware project area', () => {
    // The store lies above the firmware's project area, so only a write below that area is outside both.
    const { firmwareFirst } = uploadStore(device);
    const low = 0x100;
    expect(check(device, [store.first, firmwareFirst, store.first - 1, low]).overlays[0]!.outsideStore).toEqual([low]);
  });

  test('overlayWrites lists the written store sectors once, sorted', () => {
    const { overlay } = run([store.first - 1, store.setA, store.selB, store.end, store.selB]);
    expect(overlayWrites(overlay, device)).toEqual({ store_sectors: [store.setA, store.selB] });
  });
});

test('a torn overlay is refused', () => {
  const { output, overlay } = run([]);
  fs.writeFileSync(overlay, 'torn');
  expect(() => runtimeSafety('dt2', output, [overlay])).toThrow('not a multiple');
  expect(() => overlayWrites(overlay, 'dt2')).toThrow('not a multiple');
});
