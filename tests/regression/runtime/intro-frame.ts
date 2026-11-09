import fs from 'node:fs';
import type { Session } from '@digi/test-support';

/** A native 128 x 64 bitmap (byte `8*x + y/8`, bit `7 - y%8`) as the intro framebuffer holds it: each 8-byte column reversed byte for byte and bit for bit. */
export function introFramebuffer(native: Uint8Array): Buffer {
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

/** The safe-mode frame of a reason as the host test of `safe_screen.rs` checks it in (`core/runtime/tests/fixtures/safe-<name>.bin`), in framebuffer layout. */
export function safeFrame(name: 'key' | 'counter'): Buffer {
  const file = new URL(`../../../modules/core/runtime/tests/fixtures/safe-${name}.bin`, import.meta.url);
  return introFramebuffer(fs.readFileSync(file));
}

/** The intro's own framebuffer: the last frame the intro drew. */
export async function introNow(session: Session): Promise<Buffer> {
  const got = await session.client.framebufferGet({ source: 'intro', data: true, hash: true });
  if (got.source !== 'intro') throw new Error(`framebuffer source ${got.source}`);
  const raw = Buffer.from(got.data_b64!, 'base64');
  if (raw.length !== 1024) throw new Error(`intro framebuffer is ${raw.length} bytes`);
  return raw;
}
