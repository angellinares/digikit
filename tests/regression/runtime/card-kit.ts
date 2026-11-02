import { execFile } from 'node:child_process';
import { createHash } from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { promisify } from 'node:util';
import zlib from 'node:zlib';
import { DkError } from '@digi/dk-session';
import { LOAD_STATUS, STATUS } from '../../../packages/dk-session/src/gen_proto.ts';
import { repoRoot, type Device, type Session } from '@digi/test-support';
import { cardStore } from './device-data.ts';
import { advance, rpc, word } from './fx-live.ts';
import { planSymbol, type RuntimePlan } from './runtime-plan.ts';
import { cardOverlay, encodeOverlay, layOver, linkCard, mergeOverlays, overlayFile } from './write-safety.ts';

/**
 * Port of the card and guest helpers of tools/patchkit/pk_dkrt_boot.py (Rig.card, make_record, parse_record, boot_info, ...).
 * The store is built by the plusdrive CLI. The selection record is encoded here: the CLI writes only seq 1, counter 0, no flags
 * (selection.txt), and a test needs other values and torn copies. Layout: packages/emulator/plusdrive/src/selection.rs.
 */

const exec = promisify(execFile);
const SECTOR = 512;
const SEL_MAGIC = 0x444b534c;
const SEL_MAX = 8;
const SEL_PATH = 56;
export const E_RANGE = -46;
export const E_CARD_RO = -47;
export const CMD = { LOAD: 1, SELECT: 3, RESTART: 4, SELECT_ONE: 5, CARD_PROBE: 7 };
const MB_CMD = 0;
const MB_SEQ = 4;
const MB_RESULT = 8;
const MB_ARG = 12;

/** The plusdrive CLI: `DIGI_PLUSDRIVE` (a lane that built its own), else the shared build. */
export function plusdriveBinary(): string {
  return process.env.DIGI_PLUSDRIVE || path.join(repoRoot(), 'out/native/target/nogen/release/plusdrive');
}

// ---- the selection record (one 512-byte copy) --------------------------------------------------------------------------------------

export interface SelRecord {
  paths: string[];
  seq: number;
  counter: number;
  flags: number;
  /** Stable period of safe mode in DSP blocks (0: the default). */
  stableBlocks: number;
  /** The 8 sticky machine-type entries (`hash24 << 8 | type`, 0 is empty). */
  map: number[];
}

/** Byte offset of sticky-map entry `i`. */
const mapAt = (i: number) => (i === 0 ? 28 : 480 + 4 * (i - 1));

export function encodeRecord({
  paths,
  seq = 1,
  counter = 0,
  flags = 0,
  stableBlocks = 0,
  map = [],
}: Partial<SelRecord>): Buffer {
  if (!paths || paths.length > SEL_MAX || paths.some((p) => !p || Buffer.byteLength(p) >= SEL_PATH))
    throw new Error('selection: at most 8 paths of 1 to 55 characters');
  const b = Buffer.alloc(SECTOR);
  [SEL_MAGIC, 1, seq, counter, flags, paths.length, stableBlocks].forEach((v, i) => b.writeUInt32BE(v >>> 0, 4 * i));
  paths.forEach((p, i) => b.write(p, 32 + i * SEL_PATH));
  map.forEach((e, i) => b.writeUInt32BE(e >>> 0, mapAt(i)));
  b.writeUInt32BE(zlib.crc32(b.subarray(0, 508)), 508);
  return b;
}

/** A valid record, or null: magic, version, CRC, count and NUL-terminated paths (the runtime's rules). */
export function decodeRecord(b: Buffer): SelRecord | null {
  if (b.length !== SECTOR || b.readUInt32BE(0) !== SEL_MAGIC || b.readUInt32BE(4) !== 1) return null;
  if (b.readUInt32BE(508) !== zlib.crc32(b.subarray(0, 508))) return null;
  const count = b.readUInt32BE(20);
  if (count > SEL_MAX) return null;
  const paths: string[] = [];
  for (let i = 0; i < count; i++) {
    const raw = b.subarray(32 + i * SEL_PATH, 32 + (i + 1) * SEL_PATH);
    if (raw[0] === 0 || !raw.includes(0)) return null;
    paths.push(raw.subarray(0, raw.indexOf(0)).toString());
  }
  return {
    paths,
    seq: b.readUInt32BE(8),
    counter: b.readUInt32BE(12),
    flags: b.readUInt32BE(16),
    stableBlocks: b.readUInt32BE(24),
    map: Array.from({ length: 8 }, (_, i) => b.readUInt32BE(mapAt(i))),
  };
}

/**
 * `count` sectors at `first` of the card the guest sees: the image (sparse and about 25 GB: read by position, never whole; bytes past
 * its end read as zeros), the `overlay.bin` next to it, then the optional `overlay` blob of a session's writes.
 */
export function readSectors(file: string, first: number, count: number, overlay?: Buffer): Buffer {
  const buf = Buffer.alloc(count * SECTOR);
  const fd = fs.openSync(file, 'r');
  try {
    fs.readSync(fd, buf, 0, buf.length, first * SECTOR);
  } finally {
    fs.closeSync(fd);
  }
  layOver(buf, first, file, overlay);
  return buf;
}

/** The record a boot uses: the valid copy with the higher seq (A when equal). */
export function currentRecord([a, b]: (SelRecord | null)[]): SelRecord | null {
  return a == null || (b != null && b.seq > a.seq) ? (b ?? null) : a;
}

/** The two copies of a card, with the guest's `card.overlay` bytes (optional) laid over it; a card that ends before the area reads as zeros. */
export function recordsIn(cardFile: string, device: Device, overlay?: Buffer): (SelRecord | null)[] {
  const { selA, selB } = cardStore(device);
  return [selA, selB].map((s) => decodeRecord(readSectors(cardFile, s, 1, overlay)));
}

// ---- the card image ----------------------------------------------------------------------------------------------------------------

export interface CardOptions extends Partial<Omit<SelRecord, 'paths'>> {
  /** Work directory: the seed folder and the card directory (`<name>/card.img` and `overlay.bin`) go here. */
  dir: string;
  name?: string;
  /** Modules of the store: `name` is the store path (`modules/a.dkm`), `file` the .dkm. */
  modules?: { name: string; file: string }[];
  /** Verified DT2 firmware (.syx) that seeds the boot project; omit for none. */
  syx?: string;
  /** Selected store paths, written to both copies. Undefined or null: no record. */
  selection?: string[] | null;
  /** Each copy as given: raw bytes, null (left alone) or 'torn' (the record of the options with a bit flipped: its CRC fails). */
  recs?: [Buffer | null | 'torn', Buffer | null | 'torn'];
  /** Lay the records over a card that shares this card's image and keeps its writes, instead of building one; its sticky map is kept. */
  base?: string;
  /** Absolute sector -> bytes (a multiple of 512) written over the card, as overlay records. */
  foreign?: Record<number, Buffer>;
}

/** Python `Ctx.card` / `Rig.card`: a card with the store of `modules` and the record copies. Returns the card file. */
export async function buildCard(device: Device, o: CardOptions): Promise<string> {
  const dir = path.join(o.dir, o.name ?? 'card');
  const out = path.join(dir, 'card.img');
  fs.rmSync(dir, { recursive: true, force: true });
  if (!o.base) {
    const seed = path.join(o.dir, `seed-${o.name ?? 'card'}`);
    fs.rmSync(seed, { recursive: true, force: true });
    fs.mkdirSync(path.join(seed, 'digikit'), { recursive: true });
    fs.mkdirSync(dir, { recursive: true });
    for (const m of o.modules ?? []) {
      fs.mkdirSync(path.dirname(path.join(seed, 'digikit', m.name)), { recursive: true });
      fs.copyFileSync(m.file, path.join(seed, 'digikit', m.name));
    }
    await exec(
      plusdriveBinary(),
      ['build', seed, '-o', out, '--device', device, ...(o.syx ? ['--syx', o.syx] : ['--no-project'])],
      {
        cwd: repoRoot(),
        timeout: 60_000,
      },
    );
  }
  const sectors = new Map<number, Buffer>();
  if (o.selection != null || o.recs) {
    const map = o.map ?? (o.base ? currentRecord(recordsIn(o.base, device))?.map : undefined);
    const made = o.selection != null ? encodeRecord({ ...o, paths: o.selection, map }) : null;
    const torn = (rec: Buffer | null) => {
      const b = Buffer.from(rec ?? encodeRecord({ paths: [] }));
      b.writeUInt8(b.readUInt8(40) ^ 1, 40);
      return b;
    };
    const { selA, selB } = cardStore(device);
    (o.recs ?? [made, made]).forEach((r, i) => {
      const rec = r === 'torn' ? torn(made) : r;
      if (rec) sectors.set(i ? selB : selA, rec);
    });
  }
  for (const [first, data] of Object.entries(o.foreign ?? {}))
    for (let at = 0; at < data.length; at += SECTOR)
      sectors.set(Number(first) + at / SECTOR, data.subarray(at, at + SECTOR));
  const overlay = mergeOverlays(o.base ? cardOverlay(o.base) : Buffer.alloc(0), encodeOverlay(sectors));
  if (o.base) return linkCard(o.base, dir, overlay);
  fs.writeFileSync(overlayFile(out), overlay);
  return out;
}

/** The plusdrive option that reads a card through the `overlay.bin` next to its image. */
export const overlayArgs = (card: string): string[] =>
  fs.existsSync(overlayFile(card)) ? ['--overlay', overlayFile(card)] : [];

export interface StoreEntry {
  path: string;
  dir: boolean;
  firstSector: number;
  size: number;
  sha256: string;
}

/** `plusdrive store ls`: the header line (used sectors, version, seq, header copy) and the files with their sha256. Fails on a card with no store. */
export async function storeList(cardImage: string, device: Device): Promise<{ head: string; entries: StoreEntry[] }> {
  const { stdout } = await exec(
    plusdriveBinary(),
    ['store', 'ls', cardImage, ...overlayArgs(cardImage), '--device', device],
    {
      cwd: repoRoot(),
    },
  );
  const [head, ...lines] = stdout.trim().split('\n');
  const entries = lines.map((line) => {
    const [p, first, size, sha] = line.split('\t') as [string, string, string, string];
    const dir = p.endsWith('/');
    return {
      path: dir ? p.slice(0, -1) : p,
      dir,
      firstSector: dir ? 0 : Number(first),
      size: Number(size),
      sha256: dir ? '' : sha,
    };
  });
  return { head: head!, entries };
}

// ---- guest memory and the test mailbox ----------------------------------------------------------------------------------------------

const BI_FIELDS = [
  'magic',
  'version',
  'safe',
  'reason',
  'counter',
  'seq',
  'copy',
  'count',
  'loaded',
  'uiReached',
] as const;
const SAFE_NAMES = ['none', 'counter', 'key', 'flag'];
const CARD_FIELDS = [
  'state',
  'firstNonblank',
  'hash',
  'refused',
  'refusedRange',
  'readErrors',
  'checked',
  'claims',
  'writes',
] as const;
const CARD_NAMES = ['unknown', 'ours', 'blank', 'foreign', 'unreadable', 'too small'];
const CRASH_FIELDS = [
  'found',
  'pc',
  'vec',
  'module',
  'stable',
  'ramKept',
  'installed',
  'bad',
  'hang',
  'saved',
  'panic',
  'tail',
] as const;
/** `LOADING` in a module's boot result: its DSP part has not followed yet. */
export const E_LOADING = 1;
/** Store statuses of the upload protocol (the numbers of gen_proto's STATUS table). */
export const E_NOSTORE = -61;
export const E_INUSE = -62;
export const E_PEEK = -63;

async function words<const F extends readonly string[]>(
  session: Session,
  plan: RuntimePlan,
  symbol: string,
  fields: F,
) {
  const base = planSymbol(plan, symbol);
  const values = await Promise.all(fields.map((_, i) => word(session, base + 4 * i)));
  return Object.fromEntries(fields.map((k, i) => [k, values[i]!])) as Record<F[number], number>;
}

/** `DKRT_BOOT`: the boot's fields, per-module results (signed, `results` has `count` of them), `dspWait` and `stable`. */
export async function bootInfo(session: Session, plan: RuntimePlan) {
  const d = await words(session, plan, 'DKRT_BOOT', BI_FIELDS);
  const base = planSymbol(plan, 'DKRT_BOOT') + 4 * BI_FIELDS.length;
  const tail = await Promise.all(Array.from({ length: SEL_MAX + 2 }, (_, i) => word(session, base + 4 * i)));
  const allResults = tail.slice(0, SEL_MAX).map((v) => v | 0);
  return {
    ...d,
    results: allResults.slice(0, d.count),
    allResults,
    dspWait: tail[SEL_MAX]!,
    stable: tail[SEL_MAX + 1]!,
    reasonName: SAFE_NAMES[d.reason] ?? '?',
  };
}

/** `DKRT_CARD`: state, the first non-blank sector, its crc32, writes refused and done. */
export async function cardInfo(session: Session, plan: RuntimePlan) {
  const d = await words(session, plan, 'DKRT_CARD', CARD_FIELDS);
  return { ...d, stateName: CARD_NAMES[d.state] ?? '?' };
}

/** `DKRT_CRASH`: what the boot found of a crash record. */
export const crashInfo = (session: Session, plan: RuntimePlan) => words(session, plan, 'DKRT_CRASH', CRASH_FIELDS);

/** The two record copies as the card holds them now: the daemon's overlay over the image. */
export async function selRecords(session: Session, cardImage: string, device: Device, overlayFile: string) {
  await rpc(session, 'card.overlay', { path: overlayFile });
  return recordsIn(cardImage, device, fs.readFileSync(overlayFile));
}

export const pokeWord = (session: Session, addr: number, value: number) =>
  rpc(session, 'mem.cf.poke', { addr, value: value >>> 0 });

/** Set a firmware or runtime variable by its plan symbol (Python `M.sym` + `_poke_u32`). */
export const pokeKnob = (session: Session, plan: RuntimePlan, name: string, value: number) =>
  pokeWord(session, planSymbol(plan, name), value);

/** A mailbox command with binary arguments (big-endian words, zero padded); returns its signed result. */
export async function postRaw(session: Session, plan: RuntimePlan, cmd: number, data: Buffer): Promise<number> {
  const mb = planSymbol(plan, 'DKRT_MAILBOX');
  const arg = Buffer.concat([data, Buffer.alloc(-data.length & 3)]);
  for (let i = 0; i < arg.length; i += 4) await pokeWord(session, mb + MB_ARG + i, arg.readUInt32BE(i));
  const seq0 = await word(session, mb + MB_SEQ);
  await pokeWord(session, mb + MB_CMD, cmd);
  for (let i = 0; i < 200; i++) {
    await advance(session, 20);
    if ((await word(session, mb + MB_CMD)) === 0 && (await word(session, mb + MB_SEQ)) !== seq0) break;
  }
  return (await word(session, mb + MB_RESULT)) | 0;
}

/** Ask the runtime's card-write function to write `nbytes` at `sector`: 0, `E_RANGE` or `E_CARD_RO`. Only for sectors outside the store or on a card that is not ours. */
export const probeWrite = (session: Session, plan: RuntimePlan, sector: number, nbytes: number) => {
  const b = Buffer.alloc(8);
  b.writeUInt32BE(sector, 0);
  b.writeUInt32BE(nbytes, 4);
  return postRaw(session, plan, CMD.CARD_PROBE, b);
};

/** The selection through the mailbox (`dkrt_selection_set`); returns its result. */
export const setSelection = (session: Session, plan: RuntimePlan, paths: string[]) =>
  postRaw(session, plan, CMD.SELECT, Buffer.from(paths.join('\0') + '\0\0'));

// ---- the upload protocol ------------------------------------------------------------------------------------------------------------

/**
 * The status a protocol call ends with, as a number (0 for success). A DkSession call throws a DkError with the status name; the number comes
 * from the generated tables. Other errors are rethrown.
 */
export async function statusOf(op: () => Promise<unknown>): Promise<number> {
  try {
    await op();
    return 0;
  } catch (e) {
    const row =
      e instanceof DkError && e.kind === 'device'
        ? [...STATUS, ...LOAD_STATUS].find(([, name]) => name === e.status)
        : undefined;
    if (!row) throw e;
    return row[0];
  }
}

export const sha = (b: Uint8Array): string => createHash('sha256').update(b).digest('hex');

/** Deterministic bytes with every value (the 8-in-7 packing needs the high bits): mulberry32 of `seed`. */
export function blob(n: number, seed: number): Buffer {
  const out = Buffer.alloc(n);
  let a = seed >>> 0;
  for (let i = 0; i < n; i++) {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = Math.imul(a ^ (a >>> 15), a | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    out[i] = (t ^ (t >>> 14)) & 0xff;
  }
  return out;
}

// ---- the store as the plusdrive reader sees it ----------------------------------------------------------------------------------------

export type Files = Record<string, Buffer>;
export type Listing = Record<string, [number, string]>;

/** {path: [size, sha256]} of the files sent. */
export const listing = (files: Files): Listing =>
  Object.fromEntries(Object.entries(files).map(([p, b]) => [p, [b.length, sha(b)]]));

/** The files of a card's store as `plusdrive store ls --cat` reads them (their sha256 is checked there), and the reader's header line. A card the reader cannot read gives no files. */
export async function cardView(cardFile: string, device: Device): Promise<{ files: Files; head: string }> {
  let store: Awaited<ReturnType<typeof storeList>>;
  try {
    store = await storeList(cardFile, device);
  } catch (e) {
    return { files: {}, head: `error: ${String(e).slice(0, 200)}` };
  }
  const files: Files = {};
  for (const entry of store.entries) {
    if (entry.dir) continue;
    try {
      const cat = await exec(
        plusdriveBinary(),
        ['store', 'ls', cardFile, ...overlayArgs(cardFile), '--device', device, '--cat', entry.path],
        {
          cwd: repoRoot(),
          encoding: 'buffer',
          maxBuffer: 1 << 30,
        },
      );
      files[entry.path] = cat.stdout;
    } catch {
      files[entry.path] = Buffer.from('<unreadable>');
    }
  }
  return { files, head: store.head };
}
