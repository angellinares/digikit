import fs from 'node:fs';

/**
 * The pure part of the offline oracles over a .dkm (no patchkit): the DSP relocation, a second implementation of the loader's. dkm.ts adds
 * the Rust oracles (`patchkit needs|fit`); pack-needs.ts adds the needs of committed packs from dist/bytes/pack-verdicts.json.
 */
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
export function parseDspSection(dkm: Buffer) {
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
export function relocate(sec: ReturnType<typeof parseDspSection>, base: number): Buffer {
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

/** The page defaults of the gain fixtures (one GAIN knob, 0.5 as float bits): the words p0 .. of the committed row. */
export const GAIN_PAGE_DEFAULTS = [0x3f000000];

/** Relocation and row oracle of a one-effect module placed at BASE. */
export function fxExpectationsWith(
  file: string,
  base: number,
  needs: Record<string, number>,
  pageDefaults = GAIN_PAGE_DEFAULTS,
): FxExpectations {
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
      // the section holds placeholders: the commit writes the defaults of the effect's page knobs (init) into the row
      ...pageDefaults,
      ...Array<number>(Math.max(0, effect.defaults.length - pageDefaults.length)).fill(0),
    ],
    needs,
  };
}

/** The tags of the header table (packages/dkneeds/src/keys.rs); only NEEDS is read here. */
const TAG_NEEDS = 1;

/** The hash (`name_hash`, FNV-1a 32) of a service or export name. */
export function nameHash(name: string): number {
  let h = 0x811c9dc5;
  for (const b of Buffer.from(name)) h = Math.imul(h ^ b, 0x01000193) >>> 0;
  return h;
}

/** The service name hashes in the NEEDS entry of the header table of a packed `.dkm` (12 bytes an item: hash, minimum version, flags), in file order; none for a file without a header table. */
export function neededServiceHashes(dkm: Buffer): number[] {
  // a file with no header table (a stand-in of a host test) needs nothing
  if (dkm.length < 0x98) return [];
  const table = dkm.readUInt32BE(0x90);
  if (table + 8 > dkm.length || dkm.readUInt32BE(table) !== 0x444b5442) return [];
  let at = table + 8;
  for (let i = dkm.readUInt16BE(table + 6); i > 0; i--) {
    const count = dkm.readUInt16BE(at + 2);
    const bytes = dkm.readUInt32BE(at + 4);
    if (dkm.readUInt16BE(at) === TAG_NEEDS)
      return Array.from({ length: count }, (_, k) => dkm.readUInt32BE(at + 8 + 12 * k));
    at += 8 + bytes;
  }
  return [];
}
