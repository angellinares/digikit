// The WAV converter and the +Drive content hash (dksample.wasm, packages/dksample) and what the upload does with a file. Skips when dksample.wasm is not built
// (`mise run wasm:patchkit` builds it).
import assert from 'node:assert/strict';
import { existsSync, readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { Sample, isNative, prepareUpload } from '@digi/dk-session/dksample';

const wasm = join(resolve(dirname(fileURLToPath(import.meta.url)), '..'), 'public/dksample.wasm');
const skip = !existsSync(wasm);

/** A WAV: `bits` integer PCM (or 32-bit float with `float`), `ch` channels, `rate` Hz; sample i of channel c is `f(i, c)` in -1..1. */
const wav = ({
  rate = 48000,
  ch = 1,
  frames = 100,
  bits = 16,
  float = false,
  f = (i) => Math.sin(i / 7) * 0.5,
} = {}) => {
  const width = bits / 8;
  const pcm = Buffer.alloc(frames * ch * width);
  for (let i = 0; i < frames; i++)
    for (let c = 0; c < ch; c++) {
      const v = f(i, c);
      const at = (i * ch + c) * width;
      if (float) pcm.writeFloatLE(v, at);
      else if (bits === 8) pcm.writeUInt8(Math.round(v * 127 + 128), at);
      else if (bits === 16) pcm.writeInt16LE(Math.round(v * 32767), at);
      else if (bits === 24) pcm.writeIntLE(Math.round(v * 8388607), at, 3);
      else pcm.writeInt32LE(Math.round(v * 2147483647), at);
    }
  const h = Buffer.alloc(44);
  h.write('RIFF', 0);
  h.writeUInt32LE(36 + pcm.length, 4);
  h.write('WAVEfmt ', 8);
  h.writeUInt32LE(16, 16);
  h.writeUInt16LE(float ? 3 : 1, 20);
  h.writeUInt16LE(ch, 22);
  h.writeUInt32LE(rate, 24);
  h.writeUInt32LE(rate * ch * width, 28);
  h.writeUInt16LE(ch * width, 32);
  h.writeUInt16LE(bits, 34);
  h.write('data', 36);
  h.writeUInt32LE(pcm.length, 40);
  return Uint8Array.from(Buffer.concat([h, pcm]));
};
const sample = async () => Sample.create(readFileSync(wasm));

test('the native file: header, big-endian PCM, trailer of zeros', { skip }, async () => {
  const s = await sample();
  const c = s.convert(wav({ frames: 100 }));
  assert.equal(c.file.length, 0x40 + 200 + 16);
  assert.equal(c.rpc.length, 0x40 + 200);
  const v = new DataView(c.file.buffer);
  assert.deepEqual([c.file[0], c.file[1], v.getUint32(4), v.getUint32(8), c.file[0x14]], [0, 0, 200, 48000, 0x7f]);
  assert.equal(v.getInt16(0x40 + 2 * 10), Math.round(Math.sin(10 / 7) * 0.5 * 32767));
  assert.deepEqual(
    [...c.file.subarray(c.file.length - 16)],
    Array.from({ length: 16 }, () => 0),
  );
  assert.deepEqual(c.info, { srcRate: 48000, channels: 1, srcFrames: 100, frames: 100, dataLen: 200 });
});

test('stereo is interleaved and flagged; other widths and floats convert', { skip }, async () => {
  const s = await sample();
  const st = s.convert(wav({ ch: 2, frames: 50, f: (_i, c) => (c ? 0.25 : -0.25) }));
  assert.equal(st.file[1], 1);
  assert.equal(st.rpc.length, 0x40 + 50 * 4);
  const v = new DataView(st.file.buffer);
  assert.deepEqual([v.getInt16(0x40), v.getInt16(0x42)], [-8192, 8192]);
  for (const o of [{ bits: 8 }, { bits: 24 }, { bits: 32 }, { bits: 32, float: true }]) {
    const c = s.convert(wav({ frames: 20, f: () => 0.5, ...o }));
    assert.ok(Math.abs(new DataView(c.file.buffer).getInt16(0x40) - 16384) <= 256, JSON.stringify(o));
  }
});

test('a 44.1 kHz WAV is resampled to 48 kHz', { skip }, async () => {
  const s = await sample();
  const c = s.convert(wav({ rate: 44100, frames: 4410 }));
  assert.equal(c.info.frames, 4800);
  assert.equal(new DataView(c.file.buffer).getUint32(8), 48000);
});

test('three channels and a broken file are refused', { skip }, async () => {
  const s = await sample();
  assert.throws(() => s.convert(wav({ ch: 3 })), /not a usable WAV/);
  assert.throws(() => s.convert(Uint8Array.from(Buffer.from('RIFFxxxxWAVE'))), /not a usable WAV/);
  assert.throws(() => s.convert(new Uint8Array(0)), /not a usable WAV/);
});

test("the content hash is the firmware's: the empty file and a card vector", { skip }, async () => {
  const s = await sample();
  assert.equal(s.hash(new Uint8Array(0)), 0x43fa243b); // hash of nothing, |1 (finding 44)
  assert.equal(s.hash(new Uint8Array(16)) & 1, 1);
  assert.equal(s.hash(Uint8Array.of(1, 2, 3)), s.hash(Uint8Array.of(1, 2, 3)));
  assert.notEqual(s.hash(Uint8Array.of(1, 2, 3)), s.hash(Uint8Array.of(1, 2, 4)));
});

test('prepareUpload: a WAV is converted, a native file and any other file pass', { skip }, async () => {
  const s = await sample();
  const w = prepareUpload(s, 'Kick.WAV', wav({ frames: 10 }));
  assert.deepEqual([w.kind, w.name, w.bytes.length], ['converted', 'Kick', 0x40 + 20]);
  const full = s.convert(wav({ frames: 10 })).file;
  assert.ok(isNative(full) && isNative(full.subarray(0, full.length - 16)));
  const n = prepareUpload(s, 'k', full);
  assert.deepEqual([n.kind, n.name, n.bytes.length], ['native', 'k', 0x40 + 20]);
  const same = prepareUpload(s, 'k', full.subarray(0, full.length - 16));
  assert.equal(same.bytes.length, 0x40 + 20);
  const t = Uint8Array.from(Buffer.from('hello'));
  const p = prepareUpload(s, 'a.txt', t);
  assert.deepEqual([p.kind, p.name, p.bytes === t], ['unchanged', 'a.txt', true]);
  assert.equal(isNative(t), false);
  // what the digikit store holds is the native file of the +Drive: with its trailer, whatever the input was
  assert.deepEqual(w.stored, full);
  assert.deepEqual(n.stored, full);
  assert.deepEqual(same.stored, full);
  assert.equal(p.stored, t);
});
