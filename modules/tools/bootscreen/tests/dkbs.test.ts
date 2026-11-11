import { execFileSync, spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { crc32 } from 'node:zlib';
import { decode, encode, type Frame } from '@digi/dkbs';
import { afterAll, beforeAll, expect, test } from 'vitest';

const module = path.resolve(import.meta.dirname, '..');
let directory: string, binary: string;

interface Run {
  status: number | null;
  records: number;
  ticks: number;
  decoded: number;
  /** Per tick: the records decoded so far and the frame. */
  frames: { decoded: number; pixels: Buffer }[];
}

function run(data: Uint8Array): Run {
  const result = spawnSync(binary, { input: data, timeout: 10_000, maxBuffer: 12 + 300 * 1028 });
  expect(result.error).toBeUndefined();
  const out = result.stdout;
  if (result.status !== 0) {
    expect(out.length, 'a refused file gives no frame').toBe(0);
    return { status: result.status, records: 0, ticks: 0, decoded: 0, frames: [] };
  }
  const ticks = out.readUInt32BE(4);
  const frames = Array.from({ length: ticks }, (_, i) => ({
    decoded: out.readUInt32BE(12 + i * 1028),
    pixels: out.subarray(16 + i * 1028, 16 + i * 1028 + 1024),
  }));
  return { status: 0, records: out.readUInt32BE(0), ticks, decoded: out.readUInt32BE(8), frames };
}

/** The frame of every tick as `@digi/dkbs` decodes the file. */
function expanded(data: Uint8Array): Buffer[] {
  return decode(data).frames.flatMap((f) => Array.from({ length: f.ticks }, () => Buffer.from(f.bitmap)));
}

function expectSame(data: Uint8Array) {
  const got = run(data);
  const want = expanded(data);
  expect(got.status).toBe(0);
  expect(got.ticks).toBe(want.length);
  expect(got.records).toBe(decode(data).frames.length);
  got.frames.forEach((f, i) => expect(f.pixels.equals(want[i]!), `tick ${i}`).toBe(true));
  return got;
}

function record(hold: number, codec: number, data: Buffer, reserved = 0): Buffer {
  const header = Buffer.alloc(8);
  header.writeUInt16BE(hold);
  header.writeUInt16BE(data.length, 2);
  header[4] = codec;
  header[5] = reserved;
  return Buffer.concat([header, data]);
}
function asset(records: Buffer[], count = records.length, ticks = records.length): Buffer {
  const body = Buffer.concat(records),
    header = Buffer.alloc(16);
  header.write('DKBS');
  header.writeUInt16BE(1, 4);
  header.writeUInt16BE(count, 6);
  header.writeUInt32BE(ticks, 8);
  header.writeUInt32BE(crc32(body), 12);
  return Buffer.concat([header, body]);
}

const noise = (seed: number) => Buffer.from(Array.from({ length: 1024 }, (_, i) => (i * 73 + (i >> 3) * 17 + seed) % 256));

beforeAll(() => {
  directory = fs.mkdtempSync(path.join(os.tmpdir(), 'bootscreen-dkbs-host-'));
  binary = path.join(directory, 'dkbs-host');
  execFileSync(
    'cc',
    [
      '-std=c11',
      '-Wall',
      '-Wextra',
      '-Werror',
      '-I',
      path.join(module, 'src'),
      path.join(module, 'src/dkbs.c'),
      path.join(module, 'tests/dkbs-host.c'),
      '-o',
      binary,
    ],
    { timeout: 25_000 },
  );
}, 40_000);
afterAll(() => fs.rmSync(directory, { recursive: true, force: true }));

test('decoded frames equal the input for constant, moving and noise sets', () => {
  const constant: Frame[] = [{ bitmap: Uint8Array.from(noise(1)), ticks: 30 }];
  const moving: Frame[] = Array.from({ length: 40 }, (_, i) => {
    const bitmap = new Uint8Array(1024);
    for (let y = 0; y < 8; y++) bitmap[8 * ((i * 3) % 128) + (y % 8)] = 0xff;
    return { bitmap, ticks: 1 };
  });
  const noisy: Frame[] = Array.from({ length: 20 }, (_, i) => ({ bitmap: Uint8Array.from(noise(i * 7)), ticks: 1 }));
  for (const frames of [constant, moving, noisy]) expectSame(encode(frames));
});

test('a hold returns the frame of its record and decodes nothing again', () => {
  const a = Uint8Array.from(noise(3));
  const b = Uint8Array.from(noise(9));
  const got = expectSame(
    encode([
      { bitmap: a, ticks: 3 },
      { bitmap: b, ticks: 2 },
      { bitmap: new Uint8Array(1024), ticks: 1 },
    ]),
  );
  expect(got.ticks).toBe(6);
  expect(got.frames.map((f) => f.decoded)).toEqual([1, 1, 1, 2, 2, 3]);
  expect(got.decoded).toBe(3);
  const maximum = expectSame(encode([{ bitmap: a, ticks: 300 }]));
  expect(maximum.ticks).toBe(300);
  expect(maximum.decoded).toBe(1);
});

test('each corrupt form is refused before any frame', () => {
  const good = Buffer.from(encode([{ bitmap: Uint8Array.from(noise(5)), ticks: 2 }]));
  const black = record(1, 0, Buffer.alloc(1024));
  const bad: [string, Buffer][] = [];
  const magic = Buffer.from(good);
  magic[0] = 0;
  bad.push(['magic', magic]);
  const crc = Buffer.from(good);
  crc[12]! ^= 1;
  bad.push(['crc', crc]);
  const version = Buffer.from(good);
  version.writeUInt16BE(2, 4);
  bad.push(['version', version]);
  bad.push(['reserved byte', asset([record(1, 0, Buffer.alloc(1024), 1)])]);
  bad.push(['unknown codec', asset([record(1, 2, Buffer.alloc(1024))])]);
  bad.push(['raw size', asset([record(1, 0, Buffer.alloc(1023))])]);
  bad.push(['empty expansion', asset([record(1, 1, Buffer.alloc(0))])]);
  bad.push(['expansion over', asset([record(1, 1, Buffer.from(Array(9).fill([255, 0]).flat()))])]);
  bad.push(['expansion under', asset([record(1, 1, Buffer.from(Array(7).fill([255, 0]).flat()))])]);
  bad.push(['later record bad', asset([black, record(1, 1, Buffer.from(Array(9).fill([255, 0]).flat()))])]);
  bad.push(['hold 0', asset([record(0, 0, Buffer.alloc(1024))])]);
  bad.push(['more records than declared', asset([black], 2)]);
  bad.push(['more ticks than declared', asset([black], 1, 2)]);
  bad.push(['ticks 0', asset([black], 1, 0)]);
  bad.push(['ticks 301', asset([black], 1, 301)]);
  const trailing = Buffer.concat([good, Buffer.from([0])]);
  trailing.writeUInt32BE(crc32(trailing.subarray(16)), 12);
  bad.push(['trailing byte', trailing]);
  const short = Buffer.from(good.subarray(0, good.length - 1));
  short.writeUInt32BE(crc32(short.subarray(16)), 12);
  bad.push(['short body', short]);
  bad.push(['empty file', Buffer.alloc(0)]);
  bad.push(['too large', Buffer.alloc(65537)]);
  for (const cut of [1, 15, 16, 20, 24, good.length - 1]) bad.push([`cut at ${cut}`, good.subarray(0, cut)]);
  for (const [name, blob] of bad) expect(run(blob).status, name).toBe(1);
});
