import fs from 'node:fs';
import path from 'node:path';
import { decode, encode, type Frame } from '@digi/dkbs';
import { expect } from 'vitest';
import type { CardRig, CardSpec } from '@digi/regression/runtime/card-rig.ts';
import { encodeSettings } from '@digi/regression/runtime/project-kit.ts';

/** Shared by the bootscreen device tests (one file per group: bootscreen, bootscreen-safe, bootscreen-fault). */

export const SAFE_COUNTER = 1;
export const SAFE_KEY = 2;
const FILE = 'data/tools/bootscreen/test.dkbs';
const ACTIVE = 'tools/bootscreen/active';

const noise = (seed: number) =>
  Uint8Array.from(Array.from({ length: 1024 }, (_, i) => (i * 73 + (i >> 3) * 17 + seed) % 256));
const frames: Frame[] = [
  { bitmap: noise(1), ticks: 10 },
  { bitmap: noise(2), ticks: 10 },
  { bitmap: noise(3), ticks: 10 },
];
export const valid = encode(frames);

/** The intro framebuffer of a native bitmap: each 8-byte column is reversed byte for byte and bit for bit. */
function framebufferOf(native: Uint8Array): Buffer {
  const out = Buffer.alloc(1024);
  for (let x = 0; x < 128; x++)
    for (let j = 0; j < 8; j++) {
      let b = native[8 * x + j]!;
      b = ((b & 0xf0) >> 4) | ((b & 0x0f) << 4);
      b = ((b & 0xcc) >> 2) | ((b & 0x33) << 2);
      b = ((b & 0xaa) >> 1) | ((b & 0x55) << 1);
      out[8 * x + 7 - j] = b;
    }
  return out;
}

/** The decoded frame of the last tick, as the intro leaves it on the screen. */
export const lastFrame = framebufferOf(decode(valid).frames.at(-1)!.bitmap);

/** A card with the module, an optional data file, and the settings record with `active` set to `name` (none: no record). */
export function spec(h: CardRig, o: { name?: string; file?: Uint8Array }): CardSpec {
  const mod = h.mod('tools/bootscreen', 'bootscreen');
  const modules: { name: string; file: string }[] = [mod];
  if (o.file) {
    const file = path.join(h.artifacts, `data-${modules.length}.dkbs`);
    fs.writeFileSync(file, o.file);
    modules.push({ name: FILE, file });
  }
  const foreign: Record<number, Buffer> = {};
  if (o.name !== undefined) {
    const record = encodeSettings(1, [[ACTIVE, Buffer.from(o.name, 'latin1')]]);
    foreign[h.store.setA] = Buffer.concat([record, Buffer.alloc(512 * Math.ceil(record.length / 512) - record.length)]);
  }
  return { modules, selection: [mod.name], foreign };
}

export async function intro(h: CardRig) {
  const got = await h.session.client.framebufferGet({ source: 'intro', data: true, hash: true });
  expect(got.source).toBe('intro');
  const raw = Buffer.from(got.data_b64!, 'base64');
  expect(raw.length).toBe(1024);
  return raw;
}
