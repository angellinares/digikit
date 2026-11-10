import fs from 'node:fs';
import path from 'node:path';
import { encode, decode, type Frame } from '@digi/dkbs';
import { RT, type CardRig, type CardSpec } from '@digi/regression/runtime/card-rig.ts';
import { introFramebuffer } from '@digi/regression/runtime/intro-frame.ts';
import { encodeSettings } from '@digi/regression/runtime/project-kit.ts';

/** Shared by introblank.device.test.ts and introblank-rec.device.test.ts. */

export const SAFE_KEY = 2;
const FILE = 'data/tools/bootscreen/test.dkbs';
const ACTIVE = 'tools/bootscreen/active';

export const IB = [...RT, 'core/introblank'];

const noise = (seed: number) =>
  Uint8Array.from(Array.from({ length: 1024 }, (_, i) => (i * 73 + (i >> 3) * 17 + seed) % 256));
const frames: Frame[] = [
  { bitmap: noise(1), ticks: 10 },
  { bitmap: noise(2), ticks: 10 },
];
const valid = encode(frames);
export const lastFrame = introFramebuffer(decode(valid).frames.at(-1)!.bitmap);

/** A card with the boot screen module, a data file and the setting `active` (the settings record is laid over the card before the boot). */
export function bootScreenCard(h: CardRig): CardSpec {
  const mod = h.mod('tools/bootscreen', 'bootscreen');
  const file = path.join(h.artifacts, 'data.dkbs');
  fs.writeFileSync(file, valid);
  const record = encodeSettings(1, [[ACTIVE, Buffer.from('test.dkbs', 'latin1')]]);
  const foreign = { [h.store.setA]: Buffer.concat([record, Buffer.alloc(512 * Math.ceil(record.length / 512) - record.length)]) };
  return { modules: [mod, { name: FILE, file }], selection: [mod.name], foreign };
}
