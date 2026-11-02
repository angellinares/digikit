import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { afterEach, describe, expect, test } from 'vitest';
import { cardStore } from '../runtime/device-data.ts';
import {
  buildCard,
  currentRecord,
  decodeRecord,
  encodeRecord,
  plusdriveBinary,
  readSectors,
  recordsIn,
} from '../runtime/card-kit.ts';
import { cardAfter, encodeOverlay } from '../runtime/write-safety.ts';

const directories: string[] = [];
const tmp = () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'digi-card-kit-'));
  directories.push(dir);
  return dir;
};
afterEach(() => {
  for (const dir of directories.splice(0)) fs.rmSync(dir, { recursive: true, force: true });
});

describe('selection record', () => {
  test('round trips, with the sticky map at its two offsets', () => {
    const map = [0x11, 0, 0, 0, 0, 0, 0, 0x77];
    const rec = { paths: ['modules/a.dkm', 'modules/b.dkm'], seq: 7, counter: 2, flags: 1, stableBlocks: 300, map };
    const bytes = encodeRecord(rec);
    expect(bytes.subarray(0, 4).toString()).toBe('DKSL');
    expect(decodeRecord(bytes)).toEqual(rec);
    expect(bytes.readUInt32BE(28)).toBe(0x11);
    expect(bytes.readUInt32BE(504)).toBe(0x77);
  });

  test('a torn copy and a bad count do not decode; the higher seq wins and A wins a tie', () => {
    const bytes = encodeRecord({ paths: ['x'] });
    const torn = Buffer.from(bytes);
    torn.writeUInt8(torn.readUInt8(40) ^ 1, 40);
    expect(decodeRecord(torn)).toBeNull();
    expect(decodeRecord(Buffer.alloc(512))).toBeNull();
    expect(() => encodeRecord({ paths: Array(9).fill('x') })).toThrow();
    const [a, b] = [
      decodeRecord(encodeRecord({ paths: ['a'], seq: 2 })),
      decodeRecord(encodeRecord({ paths: ['b'], seq: 3 })),
    ];
    expect(currentRecord([a, b])).toBe(b);
    expect(currentRecord([a, a])).toBe(a);
    expect(currentRecord([null, b])).toBe(b);
  });
});

describe('card sectors', () => {
  const device = 'dn2';
  const { selA, selB } = cardStore(device);

  test('buildCard over a base places the copies, a torn copy and foreign sectors; the overlay wins over the image', async () => {
    const dir = tmp();
    const base = path.join(dir, 'base.img');
    fs.writeFileSync(base, Buffer.alloc(512));
    const foreign = Buffer.alloc(512, 0xab);
    const card = await buildCard(device, {
      dir,
      base,
      selection: ['modules/a.dkm'],
      counter: 3,
      recs: [null, 'torn'],
      foreign: { 5: foreign },
    });
    expect(recordsIn(card, device)).toEqual([null, null]);
    expect(readSectors(card, 5, 1).equals(foreign)).toBe(true);
    const both = await buildCard(device, { dir, name: 'both', base, selection: ['modules/a.dkm'], counter: 3 });
    expect(recordsIn(both, device).map((r) => r?.counter)).toEqual([3, 3]);
    // a guest write over copy B: bytes 0..3 (the magic) cleared by the mask
    const overlay = Buffer.alloc(8 + 64 + 512);
    overlay.writeBigUInt64LE(BigInt(selB));
    overlay[8] = 0x0f;
    expect(recordsIn(both, device, overlay).map((r) => r?.counter)).toEqual([3, undefined]);
    // base + overlay reads equal the bytes written, and a card after the guest's writes keeps the earlier ones
    const written = path.join(dir, 'written.bin');
    fs.writeFileSync(written, encodeOverlay(new Map([[5, Buffer.alloc(512, 0xcd)]])));
    const after = cardAfter(both, written, path.join(dir, 'after'));
    expect(readSectors(after, 5, 1).equals(Buffer.alloc(512, 0xcd))).toBe(true);
    expect(recordsIn(after, device).map((r) => r?.counter)).toEqual([3, 3]);
    expect(recordsIn(both, device).map((r) => r?.counter)).toEqual([3, 3]);
    expect(readSectors(both, 5, 1).equals(Buffer.alloc(512))).toBe(true);
    expect(selA).toBeLessThan(selB);
  });

  test.skipIf(!fs.existsSync(plusdriveBinary()))(
    'the plusdrive builder writes the bytes encodeRecord writes',
    async () => {
      const dir = tmp();
      const modules = [{ name: 'modules/a.dkm', file: path.join(dir, 'a.dkm') }];
      fs.writeFileSync(modules[0]!.file, Buffer.alloc(64));
      const card = await buildCard(device, { dir, modules });
      fs.writeFileSync(path.join(dir, 'seed-card', 'digikit', 'selection.txt'), 'modules/a.dkm\n');
      const cli = path.join(dir, 'cli.img');
      const { execFileSync } = await import('node:child_process');
      execFileSync(plusdriveBinary(), [
        'build',
        path.join(dir, 'seed-card'),
        '-o',
        cli,
        '--device',
        device,
        '--no-project',
      ]);
      expect(recordsIn(card, device)).toEqual([null, null]);
      const copies = recordsIn(cli, device);
      expect(copies[0]).toEqual(decodeRecord(encodeRecord({ paths: ['modules/a.dkm'] })));
      expect(copies[1]).toEqual(copies[0]);
    },
  );
});
