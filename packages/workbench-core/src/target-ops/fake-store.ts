// An in-memory `StoreOps` for the preview and for tests, and a builder of .dkm files with a valid header.
import { READ_MAX, fileHashHex, type DkEntry, type DkModule, type ModuleState } from '@digi/dk-session';
import type { StoreFacts, StoreOps } from './ops';
import { parseDkmHeader, usageOf } from './store';

const KIB = 1024;
const MIB = 1024 * KIB;

/** A file of `size` bytes (default 4096) with a header of docs/design/digikit-dkm.md; the other header fields are 0. `role` 1 is a service. */
export function fakeDkm(o: {
  name: string;
  role?: 0 | 1;
  version?: number;
  runtime?: number;
  size?: number;
  imageSize?: number;
  bssSize?: number;
}): Uint8Array {
  const size = Math.max(o.size ?? 4096, 0xa0);
  const b = new Uint8Array(size);
  const v = new DataView(b.buffer);
  b.set([0x44, 0x4b, 0x4d, 0x46], 0);
  v.setUint32(4, 1);
  v.setUint32(8, size);
  v.setUint32(0x0c, 0xa0);
  v.setUint32(0x30, o.runtime ?? 0);
  v.setUint32(0x34, o.version ?? 0);
  v.setUint32(0x38, o.role ?? 0);
  b.set(new TextEncoder().encode(o.name).subarray(0, 32), 0x40);
  v.setUint32(0x60, 0xa0);
  v.setUint32(0x64, o.imageSize ?? 0);
  v.setUint32(0x68, o.bssSize ?? 0);
  v.setUint32(0x6c, 4);
  // the hash slot: the file hash of the file with the slot left at zero
  const hash = fileHashHex(b);
  for (let i = 0; i < 32; i++) b[0x10 + i] = parseInt(hash.slice(2 * i, 2 * i + 2), 16);
  return b;
}

export interface FakeStoreOptions {
  /** Free bytes (default 16 MiB). */
  free?: number;
  /** Total bytes of the store (default null: unknown). */
  total?: number | null;
  facts?: Partial<StoreFacts>;
  modules?: DkModule[];
  states?: ModuleState[];
  /** A wait before every call, in ms. */
  delayMs?: number;
}

/** A store in memory. A path that ends in '/' is an empty folder (size 0, zero hash). */
export function fakeStoreOps(files: { path: string; bytes: Uint8Array }[], o: FakeStoreOptions = {}): StoreOps {
  const delay = (): Promise<void> => (o.delayMs ? new Promise((r) => setTimeout(r, o.delayMs)) : Promise.resolve());
  const find = (path: string): Uint8Array => {
    const f = files.find((x) => x.path === path && !path.endsWith('/'));
    if (!f) throw new Error(`No such file: ${path}`);
    return f.bytes;
  };
  const entries = (): DkEntry[] =>
    files
      .map((f) =>
        f.path.endsWith('/')
          ? { path: f.path, size: 0, hash: '0'.repeat(64) }
          : { path: f.path, size: f.bytes.length, hash: fileHashHex(f.bytes) },
      )
      .sort((a, b) => (a.path < b.path ? -1 : a.path > b.path ? 1 : 0));
  const read = async (path: string, offset: number, length: number) => {
    await delay();
    const bytes = find(path);
    return { size: bytes.length, bytes: bytes.slice(offset, offset + Math.min(length, READ_MAX)) };
  };
  return {
    async snapshot() {
      await delay();
      const list = entries();
      return {
        facts: {
          device: 'Fake device',
          runtime: '0.31.0',
          protocol: 1,
          caps: null,
          cardReadOnly: false,
          safeMode: false,
          imageId: null,
          svcAbi: null,
          ...o.facts,
        },
        entries: list,
        usage: usageOf(list, { free: o.free ?? 16 * MIB, total: o.total ?? null }),
        modules: o.modules ?? [],
        states: o.states ?? [],
      };
    },
    async header(path) {
      return parseDkmHeader((await read(path, 0, 0xa0)).bytes);
    },
    read,
    async readAll(path, onProgress) {
      await delay();
      const bytes = find(path);
      const out = new Uint8Array(bytes.length);
      for (let at = 0; at < bytes.length; at += READ_MAX) {
        const part = bytes.subarray(at, at + READ_MAX);
        out.set(part, at);
        onProgress?.(at + part.length, bytes.length);
      }
      return out;
    },
    async verify(path) {
      await delay();
      return fileHashHex(find(path));
    },
  };
}

const fill = (n: number, seed: number): Uint8Array => Uint8Array.from({ length: n }, (_, i) => (i * 31 + seed) & 255);
const ver = (major: number, minor: number, patch: number): number => (major << 16) | (minor << 8) | patch;

/** A believable Digitone II store for the preview: three modules (active, failed, not loaded), a service, module data and samples. */
export function demoStore(): StoreOps {
  const runtime = ver(0, 31, 0);
  const mod = (name: string, size: number) => ({
    path: `modules/${name.split('/').pop()}.dkm`,
    bytes: fakeDkm({ name, version: ver(1, 2, 0), runtime, size, imageSize: size - 0x400, bssSize: 2048 }),
  });
  const files = [
    mod('em/pluck', 24 * KIB),
    mod('em/chorus', 12 * KIB),
    mod('em/scope', 8 * KIB),
    {
      path: 'services/lfo.dks',
      bytes: fakeDkm({ name: 'core/lfo', role: 1, version: ver(0, 4, 1), runtime, size: 6 * KIB, imageSize: 5 * KIB }),
    },
    { path: 'data/em/pluck/settings.bin', bytes: fill(256, 1) },
    { path: 'data/em/chorus/presets.bin', bytes: fill(1200, 2) },
    { path: 'samples/kicks/kick-deep.wav', bytes: fill(200 * KIB, 3) },
    { path: 'samples/kicks/kick-short.wav', bytes: fill(40 * KIB, 4) },
    { path: 'samples/kicks/kick-long.wav', bytes: fill(120 * KIB, 5) },
    { path: 'samples/kicks/kick-click.wav', bytes: fill(64 * KIB, 6) },
    { path: 'samples/new/', bytes: new Uint8Array(0) },
  ];
  const module = (index: number, name: string, faults: number): DkModule => ({
    index,
    name,
    version: ver(1, 2, 0),
    needs: runtime,
    cf_base: 0x47e8b000 + index * 0x8000,
    cf_size: 24 * KIB,
    dsp_off: 0,
    dsp_len: 0,
    rows: 1,
    types: [1],
    faults,
    guard_off: false,
  });
  return fakeStoreOps(files, {
    free: 1440 * KIB,
    total: 2 * MIB,
    facts: { device: 'dn2', runtime: '0.31.0', protocol: 1, imageId: '0123456789abcdef', svcAbi: 0x5a17 },
    modules: [module(0, 'em/pluck', 0), module(1, 'em/chorus', 3)],
    states: [
      { index: 0, name: 'pluck.dkm', state: 'ACTIVE', result: 0, detail: [0, 0] },
      { index: 1, name: 'chorus.dkm', state: 'FAILED', result: -20, detail: [0, 0] },
    ],
  });
}
