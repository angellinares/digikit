import assert from 'node:assert/strict';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { crc32 } from 'node:zlib';
import { test } from 'vitest';
import { discoverExamples } from '../cli/examples.ts';
import { bitmapPixel, decode, DkbsError, encode, FRAME_BYTES, MAX_FILE_BYTES, rgbaToBitmap } from '../src/index.ts';

const dot = (x: number, y: number) => {
  const bitmap = new Uint8Array(FRAME_BYTES);
  bitmap[8 * x + ((63 - y) >> 3)]! |= 1 << (7 - ((63 - y) % 8));
  return bitmap;
};

const noise = (seed: number) => {
  const bitmap = new Uint8Array(FRAME_BYTES);
  let s = seed;
  for (let i = 0; i < FRAME_BYTES; i += 1) {
    s = (Math.imul(s, 1103515245) + 12345) >>> 0;
    bitmap[i] = s >>> 24;
  }
  return bitmap;
};

/** Build a file from raw records with a correct header and CRC; `patch` edits the header fields. */
function file(records: Uint8Array[], fields: { count?: number; ticks?: number; version?: number } = {}) {
  const body = new Uint8Array(records.reduce((n, r) => n + r.length, 0));
  let at = 0;
  for (const r of records) {
    body.set(r, at);
    at += r.length;
  }
  const out = new Uint8Array(16 + body.length);
  const view = new DataView(out.buffer);
  out.set([0x44, 0x4b, 0x42, 0x53]);
  view.setUint16(4, fields.version ?? 1);
  view.setUint16(6, fields.count ?? records.length);
  view.setUint32(8, fields.ticks ?? 1);
  view.setUint32(12, crc32(body));
  out.set(body, 16);
  return out;
}

function rawRecord(hold: number, payload: Uint8Array, codec = 0, reserved = [0, 0, 0]) {
  const out = new Uint8Array(8 + payload.length);
  new DataView(out.buffer).setUint16(0, hold);
  new DataView(out.buffer).setUint16(2, payload.length);
  out.set([codec, ...reserved], 4);
  out.set(payload, 8);
  return out;
}

const rejects = (bytes: Uint8Array, message: RegExp) =>
  assert.throws(
    () => decode(bytes),
    (e: unknown) => e instanceof DkbsError && message.test(e.message),
  );

test('round trip with moving and constant frames', () => {
  const frames = [
    ...Array.from({ length: 10 }, (_, i) => ({ bitmap: dot(i * 3, i), ticks: 1 })),
    { bitmap: dot(5, 5), ticks: 4 },
  ];
  const asset = decode(encode(frames));
  assert.equal(asset.ticks, 14);
  assert.equal(asset.frames.length, 11);
  frames.forEach((f, i) => {
    assert.deepEqual(asset.frames[i]!.bitmap, f.bitmap);
    assert.equal(asset.frames[i]!.ticks, f.ticks);
  });
});

test('identical consecutive frames merge', () => {
  const asset = decode(
    encode([
      { bitmap: dot(1, 1), ticks: 2 },
      { bitmap: dot(1, 1), ticks: 3 },
      { bitmap: dot(2, 2), ticks: 1 },
    ]),
  );
  assert.deepEqual(
    asset.frames.map((f) => f.ticks),
    [5, 1],
  );
});

test('XOR-RUN only when strictly shorter than RAW', () => {
  const sparse = encode([{ bitmap: dot(3, 3), ticks: 1 }]);
  assert.equal(sparse[16 + 4], 1);
  assert.ok(sparse.length < 16 + 8 + FRAME_BYTES);
  const dense = encode([{ bitmap: noise(1), ticks: 1 }]);
  assert.equal(dense[16 + 4], 0);
  assert.equal(dense.length, 16 + 8 + FRAME_BYTES);
  assert.deepEqual(decode(dense).frames[0]!.bitmap, noise(1));
});

test('encode rejects bad input', () => {
  assert.throws(() => encode([]), DkbsError);
  assert.throws(() => encode([{ bitmap: new Uint8Array(10), ticks: 1 }]), DkbsError);
  assert.throws(() => encode([{ bitmap: dot(0, 0), ticks: 301 }]), DkbsError);
  assert.throws(
    () =>
      encode([
        { bitmap: dot(0, 0), ticks: 200 },
        { bitmap: dot(1, 0), ticks: 101 },
      ]),
    DkbsError,
  );
});

test('encode rejects an asset over the size cap', () => {
  const frames = Array.from({ length: 80 }, (_, i) => ({ bitmap: noise(i + 1), ticks: 1 }));
  assert.throws(() => encode(frames), /maximum is 65536/);
});

test('decode rejections', () => {
  const good = file([rawRecord(1, noise(2))]);
  assert.equal(decode(good).frames.length, 1);

  rejects(good.subarray(0, 8), /truncated DKBS header/);
  const badMagic = good.slice();
  badMagic[0] = 0x58;
  rejects(badMagic, /magic\/version/);
  rejects(file([rawRecord(1, noise(2))], { version: 2 }), /magic\/version/);
  rejects(file([rawRecord(1, noise(2))], { count: 0 }), /record count/);
  rejects(file([rawRecord(1, noise(2))], { count: 301 }), /record count/);
  rejects(file([rawRecord(1, noise(2))], { ticks: 0 }), /total ticks/);
  rejects(file([rawRecord(1, noise(2))], { ticks: 301 }), /total ticks/);

  const badCrc = good.slice();
  badCrc[good.length - 1]! ^= 1;
  rejects(badCrc, /CRC32/);

  rejects(file([rawRecord(1, noise(2), 0, [0, 0, 1])]), /reserved/);
  rejects(file([rawRecord(1, noise(2), 2)]), /codec/);
  rejects(file([rawRecord(1, noise(2).subarray(0, 100))]), /payload length/);
  rejects(file([rawRecord(1, new Uint8Array(0), 1)]), /payload length/);
  rejects(file([rawRecord(0, noise(2))]), /hold ticks/);
  rejects(file([rawRecord(2, noise(2))]), /exceed declared total/);
  rejects(file([rawRecord(1, noise(2))], { ticks: 2 }), /mismatched total/);

  // XOR-RUN that expands to more than 1024 bytes, and one that is too short
  rejects(
    file([
      rawRecord(
        1,
        Uint8Array.from([0xff, 0, 0xff, 0, 0xff, 0, 0xff, 0, 0xff, 0, 0xff, 0, 0xff, 0, 0xff, 0, 0xff, 0]),
        1,
      ),
    ]),
    /XOR-RUN/,
  );
  rejects(file([rawRecord(1, Uint8Array.from([0xff, 0]), 1)]), /XOR-RUN/);

  rejects(file([rawRecord(1, noise(2)).subarray(0, 100)], { count: 1 }), /truncated record payload/);
  const trailing = file([rawRecord(1, noise(2)), Uint8Array.from([9, 9])], { count: 1 });
  rejects(trailing, /trailing/);
  rejects(new Uint8Array(MAX_FILE_BYTES + 1), /maximum is 65536/);
});

test('rgbaToBitmap layout and threshold', () => {
  const rgba = new Uint8Array(128 * 64 * 4);
  const set = (x: number, y: number, r: number, g: number, b: number, a = 255) =>
    rgba.set([r, g, b, a], 4 * (y * 128 + x));
  set(0, 0, 255, 255, 255);
  set(127, 63, 255, 255, 255);
  set(10, 9, 255, 255, 255);
  set(20, 20, 128, 128, 128); // luma 128: not above the default threshold
  set(21, 20, 129, 129, 129);
  set(30, 30, 255, 255, 255, 0); // transparent: black
  const bitmap = rgbaToBitmap(rgba);
  assert.ok(bitmapPixel(bitmap, 0, 0));
  assert.equal(bitmap[7], 0x01);
  assert.equal(bitmap[1016], 0x80);
  assert.equal(bitmap[86], 0x02);
  assert.ok(!bitmapPixel(bitmap, 20, 20));
  assert.ok(bitmapPixel(bitmap, 21, 20));
  assert.ok(!bitmapPixel(bitmap, 30, 30));
  assert.ok(bitmapPixel(rgbaToBitmap(rgba, 100), 20, 20));
});

test('pixel positions use the panel vertical order, y = 0 at the top', () => {
  const lit = (x: number, y: number) => {
    const rgba = new Uint8Array(128 * 64 * 4);
    rgba.fill(0);
    rgba.set([255, 255, 255, 255], 4 * (y * 128 + x));
    return rgbaToBitmap(rgba);
  };
  const only = (bitmap: Uint8Array, index: number, mask: number) => {
    bitmap.forEach((v, i) => assert.equal(v, i === index ? mask : 0, `byte ${i}`));
  };
  const a = lit(0, 0);
  only(a, 7, 0x01);
  const b = lit(127, 63);
  only(b, 1016, 0x80);
  const c = lit(0, 63);
  only(c, 0, 0x80);
  assert.equal(bitmapPixel(a, 0, 0), true);
  assert.equal(bitmapPixel(a, 0, 63), false);
  assert.equal(bitmapPixel(c, 0, 63), true);
  for (const [x, y] of [
    [5, 17],
    [64, 32],
    [127, 0],
    [100, 63],
  ] as const) {
    const m = lit(x, y);
    for (let yy = 0; yy < 64; yy++) assert.equal(bitmapPixel(m, x, yy), yy === y);
  }
});

test('discoverExamples keeps files that export ticks and draw and skips helpers', async () => {
  const dir = mkdtempSync(path.join(tmpdir(), 'dkbs-examples-'));
  try {
    writeFileSync(path.join(dir, 'a.mjs'), 'export const ticks = 1; export const draw = () => {};');
    writeFileSync(path.join(dir, 'helper.mjs'), 'export const story = () => {};');
    writeFileSync(path.join(dir, 'broken.mjs'), 'export const ticks = ;');
    writeFileSync(path.join(dir, 'notes.txt'), 'x');
    const found = await discoverExamples(dir);
    assert.deepEqual(
      found.map((e) => e.name),
      ['a', 'broken'],
    );
    assert.equal(found[0]!.error, undefined);
    assert.ok(found[1]!.error);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
