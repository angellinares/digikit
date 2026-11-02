// The ready-state cache logic under Node with an in-memory store, on the DN2
// ready inputs when present: the round trip is exact, key and version
// handling work, and the timings of saving and restoring are printed.
//   node --test apps/web/tests/ready-cache.test.mjs (run by pnpm --filter @digi/web test)
import fs from 'node:fs';
import path from 'node:path';
import assert from 'node:assert/strict';
import { fileURLToPath } from 'node:url';
import { ReadyCache, memoryStore, sha256Hex, CORE_VERSION } from '../src/ready-cache.ts';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../..');
const ready = path.resolve(
  root,
  process.env.DIGI_EMU_READY_DIR ??
    JSON.parse(fs.readFileSync(path.join(root, 'tools/emulator/ready_inputs.json'), 'utf8')).ready_dir,
);
const read = (n) => new Uint8Array(fs.readFileSync(path.join(ready, n)));
const state = fs.existsSync(path.join(ready, 'digi-audio-m5.snap'))
  ? {
      image: read('digi-audio-dn2-image.bin'),
      dsp: read('digi-audio-m5.snap.dsp'),
      snapshot: read('digi-audio-m5.snap'),
    }
  : { image: new Uint8Array([1, 2, 3]), dsp: new Uint8Array(1000).fill(7), snapshot: new Uint8Array(5000) };
const syx = new Uint8Array([9, 9, 9]);

const store = memoryStore();
const cache = new ReadyCache(store);
const key = await sha256Hex(syx);
assert.equal(key.length, 64);
assert.equal(await cache.load(key), undefined);
let t = performance.now();
const stored = await cache.save(key, state);
const saveMs = performance.now() - t;
t = performance.now();
const back = await cache.load(key);
const loadMs = performance.now() - t;
for (const k of ['image', 'dsp', 'snapshot']) assert.ok(Buffer.from(back[k]).equals(Buffer.from(state[k])), k);
assert.equal(await cache.load('0'.repeat(64)), undefined);
// A corrupt entry is dropped, not returned.
await store.put(ReadyCache.key(key), new Uint8Array([1, 2, 3, 4]));
assert.equal(await cache.load(key), undefined);
assert.equal(await store.get(ReadyCache.key(key)), undefined);
const raw = state.image.length + state.dsp.length + state.snapshot.length;
console.log(
  `ok (core version ${CORE_VERSION}): ${(raw / 1e6).toFixed(1)} MB state -> ${(stored / 1e6).toFixed(1)} MB stored, save ${saveMs.toFixed(0)} ms, restore ${loadMs.toFixed(0)} ms`,
);
