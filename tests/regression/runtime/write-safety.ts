import fs from 'node:fs';
import path from 'node:path';
import { cloneFile, type Device } from '@digi/test-support';
import { cardStore, expansionRegion, uploadStore } from './device-data.ts';
import type { RuntimePlan } from './runtime-plan.ts';

/** Write-watch and card-overlay safety of the runtime proofs. */

// A `card.overlay` file holds, per sector written by the guest: u64 le sector, 64-byte mask (bit i of byte i / 8 names
// data byte i), 512 data bytes.
const SECTOR = 512;
const RECORD = 8 + 64 + SECTOR;
// Sectors of one settings copy (tools/device/dkproto.py SETTINGS_SECTORS).
const SETTINGS_SECTORS = 16;
// Kit-library sector ranges of the firmware (hi exclusive), allowed by the fx-kit and fx-routes policies.
export const KIT_AREAS: Record<Device, [number, number][]> = { dt2: [[0x16000, 0x17000]], dn2: [[0x2000, 0x2100]] };

export type SafetyPolicy = 'default' | 'fx-kit' | 'fx-routes';

export function* overlayRecords(blob: Buffer) {
  if (blob.length % RECORD) throw new Error(`overlay size ${blob.length} is not a multiple of ${RECORD}`);
  for (let at = 0; at < blob.length; at += RECORD)
    yield {
      sector: Number(blob.readBigUInt64LE(at)),
      mask: blob.subarray(at + 8, at + 72),
      data: blob.subarray(at + 72, at + RECORD),
    };
}

const overlaySectors = (file: string) => Array.from(overlayRecords(fs.readFileSync(file)), (r) => r.sector);

/** Sectors that lie in the module store of the device, sorted. */
function cardStoreHits(sectors: number[], device: Device): number[] {
  const { first, end } = cardStore(device);
  return sectors.filter((s) => first <= s && s < end).sort((a, b) => a - b);
}

/** Written module store sectors of an overlay, including the settings and selection copies. */
export function overlayWrites(overlay: string, device: Device): { store_sectors: number[] } {
  return { store_sectors: [...new Set(cardStoreHits(overlaySectors(overlay), device))] };
}

type Masked = { mask: Buffer; data: Buffer };

/** Overlay records for the sectors, sorted: the Rust parser wants strictly rising sectors. */
function pack(sectors: Map<number, Masked>): Buffer {
  const out = Buffer.alloc(sectors.size * RECORD);
  [...sectors]
    .sort((a, b) => a[0] - b[0])
    .forEach(([sector, { mask, data }], i) => {
      out.writeBigUInt64LE(BigInt(sector), i * RECORD);
      mask.copy(out, i * RECORD + 8);
      data.copy(out, i * RECORD + 72);
    });
  return out;
}

/** An overlay of whole sectors (sector number -> 512 bytes). */
export const encodeOverlay = (sectors: Map<number, Buffer>): Buffer =>
  pack(new Map([...sectors].map(([sector, data]) => [sector, { mask: Buffer.alloc(64, 0xff), data }])));

type Records = Map<number, { mask: Buffer; data: Buffer }>;
const parsed = new Map<string, { key: string; records: Records }>();

/** The records of an overlay file by sector. They are kept until the file changes: a reader asks for many sectors. */
function overlayOf(file: string): Records {
  if (!fs.existsSync(file)) return new Map();
  const st = fs.statSync(file);
  const key = `${st.size}:${st.mtimeMs}`;
  let hit = parsed.get(file);
  if (hit?.key !== key) {
    hit = { key, records: new Map(Array.from(overlayRecords(fs.readFileSync(file)), (r) => [r.sector, r])) };
    parsed.set(file, hit);
  }
  return hit.records;
}

/** The sectors FIRST on of the card the guest sees: BUF holds the image bytes, the `overlay.bin` next to it and then the optional EXTRA overlay blob go over them. */
export function layOver(buf: Buffer, first: number, card: string, extra?: Buffer): void {
  const records = [
    overlayOf(overlayFile(card)),
    new Map(Array.from(overlayRecords(extra ?? Buffer.alloc(0)), (r) => [r.sector, r])),
  ];
  for (let n = 0; n * SECTOR < buf.length; n++)
    for (const map of records) {
      const r = map.get(first + n);
      if (r) for (let i = 0; i < SECTOR; i++) if (r.mask[i >> 3]! & (1 << (i & 7))) buf[n * SECTOR + i] = r.data[i]!;
    }
}

/**
 * The writes of a session: the records of an exported overlay minus those that only repeat OWN, the `overlay.bin` the card was loaded with
 * (the daemon exports its whole overlay). A guest write of the very bytes the card already held is not told apart from them.
 */
export function withoutOwn(exported: Buffer, own: Buffer): Buffer {
  const mine = new Map(Array.from(overlayRecords(own), (r) => [r.sector, r]));
  const repeats = (r: { sector: number; mask: Buffer; data: Buffer }) => {
    const m = mine.get(r.sector);
    if (!m) return false;
    for (let i = 0; i < SECTOR; i++)
      if (r.mask[i >> 3]! & (1 << (i & 7)) && (!(m.mask[i >> 3]! & (1 << (i & 7))) || m.data[i] !== r.data[i]))
        return false;
    return true;
  };
  return Buffer.concat(
    Array.from(overlayRecords(exported))
      .filter((r) => !repeats(r))
      .map((r) => pack(new Map([[r.sector, r]]))),
  );
}

/** One overlay of several, in order: later bytes win. */
export function mergeOverlays(...blobs: Buffer[]): Buffer {
  const all = new Map<number, Masked>();
  for (const blob of blobs)
    for (const r of overlayRecords(blob)) {
      const cur = all.get(r.sector) ?? { mask: Buffer.alloc(64), data: Buffer.alloc(SECTOR) };
      for (let i = 0; i < SECTOR; i++)
        if (r.mask[i >> 3]! & (1 << (i & 7))) {
          cur.data[i] = r.data[i]!;
          cur.mask[i >> 3]! |= 1 << (i & 7);
        }
      all.set(r.sector, cur);
    }
  return pack(all);
}

/** The `overlay.bin` that sits next to a card image: the daemon applies it on load. */
export const overlayFile = (card: string): string => path.join(path.dirname(card), 'overlay.bin');

export const cardOverlay = (card: string): Buffer =>
  fs.existsSync(overlayFile(card)) ? fs.readFileSync(overlayFile(card)) : Buffer.alloc(0);

/**
 * A card is a directory with `card.img` and its own `overlay.bin`. `link` makes one that shares the base image (a hard link: no copy;
 * `cloneFile` only where linking fails, e.g. across volumes) and holds `overlay` as its writes. Returns the new `card.img`.
 */
export function linkCard(base: string, dir: string, overlay: Buffer): string {
  const card = path.join(dir, 'card.img');
  fs.mkdirSync(dir, { recursive: true });
  fs.rmSync(card, { force: true });
  try {
    fs.linkSync(base, card);
  } catch {
    cloneFile(base, card);
  }
  fs.writeFileSync(overlayFile(card), overlay);
  return card;
}

/** The card as the guest left it: a new card in DIR with the writes of BASE, then those of the session's exported OVERLAY file. */
export function cardAfter(base: string, overlayFile: string, dir: string): string {
  return linkCard(base, dir, mergeOverlays(cardOverlay(base), fs.readFileSync(overlayFile)));
}

const hex = (n: number) => `0x${n.toString(16)}`;

/** The first symbol `<owner>@...:<name>` of the plan, if any. */
function ownedSymbol(plan: RuntimePlan, owner: string, name: string): number | undefined {
  for (const [key, value] of Object.entries(plan.symbols))
    if (key.startsWith(`${owner}@`) && key.slice(key.indexOf(':') + 1) === name) return value;
  return undefined;
}

/** Card reads land by DMA in these buffers of core/runtime (`[start, size]`, sorted); an image of an older runtime lacks some of them. */
export function dmaHoles(plan: RuntimePlan): [number, number][] {
  const holes: [number, number][] = [];
  const hole = (name: string, size: number) => {
    const start = ownedSymbol(plan, 'core/runtime', name);
    if (start !== undefined) holes.push([start, size]);
    return start !== undefined;
  };
  if (!hole('DKRT_STAGE', 64 * 1024)) throw new Error('no symbol core/runtime:DKRT_STAGE');
  hole('DKRT_SECBUF', 512);
  hole('DKRT_UPLOAD_BUF', 36896);
  hole('DKRT_SDK_BUF', 17408); // the SDK file calls' table streams (card reads land by DMA)
  hole('DKRT_IO_TASK', 16496); // the I/O task's stack, TCB and semaphore (the kernel writes them)
  hole('DKRT_STORE_PEEK', 512);
  hole('DKRT_STORE_FP', 512);
  const pdw = ownedSymbol(plan, 'core/plusdrive-write', 'DKPW_DMA'); // the +Drive adapter's buffers that the firmware's file code writes
  if (pdw !== undefined) holes.push([pdw, 640]);
  // The header sector of the settings record is a hole only together with the record.
  if (hole('DKRT_SETTINGS', SETTINGS_SECTORS * SECTOR)) hole('DKRT_SETTINGS_HDR', 512);
  return holes.sort((a, b) => a[0] - b[0] || a[1] - b[1]);
}

/** The write-watch environment of the runtime proof: the module RAM minus the DMA holes of core/runtime, and the PCs of the modules that may write. */
export function runtimeWatchEnv(planFile: string, out: string): Record<string, string> {
  const plan: RuntimePlan & { regions: Record<string, number[]> } = JSON.parse(fs.readFileSync(planFile, 'utf8'));
  const [lo, hi] = expansionRegion(plan.base.device as Device, plan.base.version);
  const ranges: string[] = [];
  let at = lo;
  for (const [start, size] of dmaHoles(plan)) {
    if (start > at) ranges.push(`${hex(at)}-${hex(start)}`); // adjacent holes leave no range between them
    at = start + size;
  }
  ranges.push(`${hex(at)}-${hex(hi)}`);
  return {
    DIGI_WRITE_WATCH: ranges.join(','),
    DIGI_WRITE_WATCH_AFTER: 'cacr',
    DIGI_WRITE_WATCH_ALLOW_PC: `${hex(lo)}-${hex(lo + plan.regions['cf.expansion']![0]!)}`,
    DIGI_FLUSH_CHECK: '1',
    DIGI_WRITE_WATCH_OUT: out,
    // One numbered log and summary per emulator, as runtimeSafety expects.
    DIGI_WRITE_WATCH_SEQ: '1',
  };
}

function runtimePolicy(device: Device, name: SafetyPolicy) {
  if (name !== 'default' && name !== 'fx-kit' && name !== 'fx-routes')
    throw new Error(`unknown runtime safety policy: ${String(name)}`);
  const store = cardStore(device);
  return {
    name,
    firmware_areas: name === 'default' ? [] : KIT_AREAS[device],
    store_extra: name === 'fx-routes' ? [store.setA, store.setB] : [],
  };
}

/** The card sectors an overlay may not contain: store sectors other than the selection copies and the policy's extras, and low sectors other than those and the kit areas. */
function runtimeCardPolicy(sectors: number[], device: Device, areas: [number, number][], extra: number[]) {
  const { selA, selB, first, end, firmwareFirst } = uploadStore(device);
  const allowed = [selA, selB];
  const unique = [...new Set(sectors)];
  return {
    // Below the firmware's own project sectors and outside the module store range.
    outsideStore: unique.filter((s) => s < firmwareFirst && !(first <= s && s < end)).sort((a, b) => a - b),
    invalid_store: [...new Set(cardStoreHits(sectors, device))]
      .filter((s) => !allowed.includes(s) && !extra.includes(s))
      .sort((a, b) => a - b),
    invalid_low: unique
      .filter((s) => s < firmwareFirst && !allowed.includes(s) && !areas.some(([lo, hi]) => lo <= s && s < hi))
      .sort((a, b) => a - b),
  };
}

/** After the owned processes exit: every sequenced watch log needs a complete clean summary, and every overlay a clean card policy. */
export function runtimeSafety(
  device: Device,
  output: string,
  overlays: string[],
  minEmulators = 1,
  policy: SafetyPolicy = 'default',
  storeExtra: readonly number[] = [],
) {
  if (minEmulators < 1 || !overlays.length)
    throw new Error('runtime safety needs observed emulators and exported overlays');
  const facts = runtimePolicy(device, policy);
  facts.store_extra = [...new Set([...facts.store_extra, ...storeExtra])];
  const { dir, name: stem } = path.parse(output);
  const indexed = (suffix: string) => {
    const files = new Map<number, string>();
    for (const file of fs.readdirSync(dir)) {
      if (!file.startsWith(`${stem}-`) || !file.endsWith(`.${suffix}`)) continue;
      const index = file.slice(stem.length + 1, file.length - suffix.length - 1);
      if (!/^\d+$/.test(index)) throw new Error(`malformed watch sequence filename: ${path.join(dir, file)}`);
      files.set(Number(index), path.join(dir, file));
    }
    return files;
  };
  const logs = indexed('jsonl');
  const summaries = indexed('summary.json');
  if (
    logs.size < minEmulators ||
    [...logs.keys()].some((k) => k >= logs.size) ||
    logs.size !== summaries.size ||
    [...logs.keys()].some((k) => !summaries.has(k))
  )
    throw new Error('missing or incomplete sequenced emulator summaries');
  let events = 0;
  let flush = 0;
  let cpushl = 0;
  for (const file of summaries.values()) {
    const data = JSON.parse(fs.readFileSync(file, 'utf8')) as Record<string, unknown>;
    for (const key of ['events', 'cpushl'] as const)
      if (!Number.isInteger(data[key]) || (data[key] as number) < 0) throw new Error(`invalid ${key} in ${file}`);
    if (!Array.isArray(data.flush_hits)) throw new Error(`invalid flush_hits in ${file}`);
    events += data.events as number;
    flush += data.flush_hits.length;
    cpushl += data.cpushl as number;
  }
  const cards = overlays.map((file) => {
    const sectors = overlaySectors(file);
    return {
      path: file,
      sectors: sectors.length,
      ...runtimeCardPolicy(sectors, device, facts.firmware_areas, facts.store_extra),
    };
  });
  return {
    policy: facts,
    emulators: summaries.size,
    write_events: events,
    flush_hits: flush,
    cpushl,
    overlays: cards,
    pass: events === 0 && flush === 0 && cards.every((c) => !c.invalid_store.length && !c.invalid_low.length),
  };
}
