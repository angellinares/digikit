// Fake modules and the needs stand-in of the Transfer logic tests (dktransfer.test.ts, dktransfer.wasm.test.ts).
import type { FitReader } from '@digi/workbench-core/modules/fit.ts';
import type { Needs } from '@digi/workbench-core/modules/fit.ts';

export interface DkmOptions {
  machine?: number;
  fx?: number;
  size?: number;
  dsp?: number;
  version?: number;
  salt?: number;
  /** The header table version (default 1). */
  tableVersion?: number;
  /** The role word at header 0x38: 0 module, 1 service (default 0). */
  role?: number;
}

/** A .dkm with a valid header (docs/design/digikit-dkm.md) of `size` bytes. */
export function dkm(
  name: string,
  {
    machine = 0,
    fx = 0,
    size = 700,
    dsp = 0,
    version = 0x000102,
    salt = 0,
    tableVersion = 1,
    role = 0,
  }: DkmOptions = {},
): Uint8Array {
  const b = new Uint8Array(size);
  b.set([0x44, 0x4b, 0x4d, 0x46], 0);
  new DataView(b.buffer).setUint32(8, size);
  new DataView(b.buffer).setUint32(0x34, version);
  b.set(Buffer.from(name), 0x40);
  new DataView(b.buffer).setUint32(0x64, 100);
  new DataView(b.buffer).setUint32(0x68, 20);
  new DataView(b.buffer).setUint32(0x9c, dsp);
  for (let i = 0xa0; i < size; i++) b[i] = (i * 31 + salt) & 255;
  // the header table (header 0x90/0x94): magic DKTB, version 1, then a KEYS entry (tag 3) with a group for each kind that has keys (machine kind 2, effect kind 4)
  const groups: [number, number][] = [];
  if (machine) groups.push([2, machine]);
  if (fx) groups.push([4, fx]);
  const v = new DataView(b.buffer);
  let at = 0xa0;
  v.setUint32(at, 0x444b5442);
  v.setUint16(at + 4, tableVersion);
  v.setUint32(0x38, role);
  v.setUint16(at + 6, groups.length ? 1 : 0);
  at += 8;
  if (groups.length) {
    const len = groups.reduce((n, g) => n + 8 + 4 * g[1], 0);
    v.setUint16(at, 3);
    v.setUint16(at + 2, groups.length);
    v.setUint32(at + 4, len);
    at += 8;
    b.fill(0, at, at + len);
    for (const [kind, n] of groups) {
      v.setUint32(at, kind);
      v.setUint32(at + 4, n);
      at += 8 + 4 * n;
    }
  }
  v.setUint32(0x90, 0xa0);
  v.setUint32(0x94, at - 0xa0);
  return b;
}

/**
 * The needs reader of these tests. The needs are inferred by Rust (packages/dkneeds, through patchkit.wasm: dkneeds.wasm.test.ts runs the real one on
 * real files); a fake module here has only a header, so a stand-in gives the fit logic its input: the arena block of image 100 and bss 20, and the
 * DSP area the fake's DSP size word holds.
 */
export const needsOf = (b: Uint8Array): Needs & { file_size: number } => {
  const dsp = new DataView(b.buffer, b.byteOffset, b.length).getUint32(0x9c);
  return { file_size: b.length, ARENA: 120, DSP_AREA: dsp, DSP_ALIGN: dsp ? 8 : 0 };
};

/**
 * A fit rule that finds every module fits, for the host tests that need a reader but do not judge fit (the rule is Rust, run for real by the
 * `*.wasm.test.ts` files).
 */
export const everythingFits: FitReader = {
  fit: ({ needs }) => ({ each: needs.map(() => null), set: null, line: '' }),
};
