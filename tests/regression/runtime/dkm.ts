import fs from 'node:fs';
import { patchkit } from './patchkit.ts';

/** Offline oracles over a .dkm. The needs and the fit rule are the Rust ones (`patchkit needs|fit`); the DSP relocation is a second implementation. */
const L2_BYTE_BASE = 0x2000_0000;
const L2_SW_BASE = 0x00b8_0000;

export interface FxExpectations {
  image_hex: string;
  row: number[];
  needs: Record<string, number>;
}

export interface ModuleRange {
  dsp_off: number;
  dsp_len: number;
}

interface FxRecord {
  process: number;
  desc: number;
  defaults: number[];
}

/** The DSP section of a .dkm (docs/design/digikit-dkm.md); only what the effect oracle reads. */
function parseDspSection(dkm: Buffer) {
  const start = dkm.readUInt32BE(0x98);
  const s = dkm.subarray(start, start + dkm.readUInt32BE(0x9c));
  if (s.readUInt32BE(0) !== 0x444b4453) throw new Error('no DSP section');
  const imageOff = s.readUInt32BE(0x18);
  const relocOff = s.readUInt32BE(0x1c);
  const relocs = Array.from({ length: s.readUInt32BE(0x20) }, (_, i) => {
    const word = s.readUInt32BE(relocOff + 8 * i + 4);
    return { offset: s.readUInt32BE(relocOff + 8 * i), kind: word >>> 28, cls: (word >>> 24) & 15 };
  });
  // Version 4 has effect records of 64 bytes: process, desc, state_bytes, n_params, flags, p0 .. p7.
  const fxs: FxRecord[] = [];
  if (s.readUInt32BE(4) === 4) {
    for (let i = 0; i < s.readUInt32BE(0x38); i++) {
      const at = s.readUInt32BE(0x34) + 64 * i;
      fxs.push({
        process: s.readUInt32BE(at),
        desc: s.readUInt32BE(at + 4),
        defaults: Array.from({ length: 8 }, (_, k) => s.readUInt32BE(at + 20 + 4 * k)),
      });
    }
  }
  return {
    image: Buffer.from(s.subarray(imageOff, imageOff + s.readUInt32BE(0x0c))),
    bss: s.readUInt32BE(0x10),
    nimports: s.readUInt32BE(0x28),
    relocs,
    fxs,
  };
}

/** The image placed at BASE; DSP imports are not supported (the oracle refuses a module that has them). */
function relocate(sec: ReturnType<typeof parseDspSection>, base: number): Buffer {
  const m = Buffer.from(sec.image);
  const byteShift = base - L2_BYTE_BASE;
  const pcShift = Math.floor(byteShift / 2);
  for (const { offset: o, kind, cls } of sec.relocs) {
    if (cls > 1) throw new Error('DSP imports are not supported');
    const add = cls === 0 ? pcShift : byteShift;
    if (kind === 0) {
      const v = ((m[o + 2]! << 16) | m.readUInt16LE(o + 4)) + add;
      if (v >= 1 << 24) throw new Error('relocated address leaves 24 bits');
      m[o + 2] = (v >> 16) & 0xff;
      m.writeUInt16LE(v & 0xffff, o + 4);
    } else {
      const v = (m.readUInt16LE(o + 2) * 0x10000 + m.readUInt16LE(o + 4) + add) >>> 0;
      m.writeUInt16LE(v >>> 16, o + 2);
      m.writeUInt16LE(v & 0xffff, o + 4);
    }
  }
  return m;
}

/** What a load of each file takes, from `patchkit needs --json`; throws for a file the loader would refuse. */
export async function needsOf(files: string[]): Promise<Record<string, number>[]> {
  const { stdout } = await patchkit(['needs', '--json', ...files]).catch((error: { stdout?: string }) => {
    if (error.stdout) return { stdout: error.stdout };
    throw error;
  });
  const out = JSON.parse(stdout) as Record<string, Record<string, number> & { error?: string }>;
  return files.map((file) => {
    if ('error' in out[file]!) throw new Error(`patchkit needs ${file}: ${out[file]!.error}`);
    return out[file]!;
  });
}

/** The fit rule (`patchkit fit --json -`) for NEEDS against CAPS; EMPTY makes CAPS the capacity of a runtime that loaded nothing. */
export async function fit(caps: unknown, needs: Record<string, number>[], empty = false) {
  const { stdout } = await patchkit(['fit', '--json', '-'], { input: JSON.stringify({ caps, needs, empty }) });
  return JSON.parse(stdout);
}

/** Relocation and row oracle of a one-effect module placed at BASE. */
export async function fxExpectations(file: string, base: number): Promise<FxExpectations> {
  const sec = parseDspSection(fs.readFileSync(file));
  if (sec.fxs.length !== 1 || sec.nimports)
    throw new Error('FX oracle requires exactly one effect without DSP imports');
  const effect = sec.fxs[0]!;
  const stateAt = (sec.image.length + sec.bss + 7) & ~7;
  return {
    image_hex: relocate(sec, base).toString('hex'),
    row: [
      L2_SW_BASE + Math.floor((base + effect.process - L2_BYTE_BASE) / 2),
      base + effect.desc,
      base + stateAt,
      1,
      ...effect.defaults,
    ],
    needs: (await needsOf([file]))[0]!,
  };
}

/** The DSP ranges of the modules are pairwise disjoint. */
function rangesOk(mods: ModuleRange[]): boolean {
  const rs = mods.filter((m) => m.dsp_len).map((m) => [m.dsp_off, m.dsp_off + m.dsp_len] as const);
  rs.sort((a, b) => a[0] - b[0] || a[1] - b[1]);
  return rs.every((r, i) => i === rs.length - 1 || r[1] <= rs[i + 1]![0]);
}

/** Range and native fit oracles of the full FX chain: the last effect against the live capacity, all of them against an empty device. */
export async function chainCheck(snapshot: { modules: ModuleRange[]; capacity: unknown }, effectFiles: string[]) {
  const needs = await needsOf(effectFiles);
  return {
    ranges_ok: rangesOk(snapshot.modules),
    live: await fit(snapshot.capacity, [needs.at(-1)!]),
    empty: await fit(snapshot.capacity, needs, true),
  };
}
