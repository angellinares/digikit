import fs from 'node:fs';
import zlib from 'node:zlib';
import { deviceError, type DkSession } from '@digi/dk-session';
import { CMD, READ_MAX } from '@digi/dk-session/gen_proto';
import { button, encoder, type Device, type Session } from '@digi/test-support';
import { expect } from 'vitest';
import { postRaw, readSectors } from './card-kit.ts';
import { advance, bounded, dspBytes, word } from './fx-live.ts';
import { planSymbol, type RuntimePlan } from './runtime-plan.ts';
import { overlayRecords } from './write-safety.ts';

/**
 * Helpers of the settings, roundtrip and equiv device tests (ports of 6a5bb1ed:tools/patchkit/pk_digikit_settings.py, pk_digikit_project.py and pk_digikit_scen.py):
 * the settings record codec and mailbox, the project slot and the machine tables. The card fixture is card-rig.ts.
 * Candidates for card-kit.ts: `settingsRecord`, `settingsMailbox`.
 */

// ---- the settings record (port of tools/device/dksettings.py; vectors in sdk/vectors/settings-vectors.json) -------------------------

const HEADER = 18;
/** One settings copy: 16 sectors (sdk/sdk.toml [settings]). */
export const SETTINGS_AREA = 16 * 512;
export const SETTINGS_KEY_MAX = 47;
export const SETTINGS_VALUE_MAX = 1024;

export type SettingsEntry = [key: string, value: Buffer];
export interface SettingsRecord {
  seq: number;
  used: number;
  entries: SettingsEntry[];
}

const crcOf = (b: Buffer, used: number) => zlib.crc32(Buffer.concat([b.subarray(0, 14), b.subarray(HEADER, used)]));

const validKey = (key: Buffer) =>
  key.length >= 1 && key.length <= SETTINGS_KEY_MAX && key.every((c) => c >= 0x21 && c <= 0x7e);

/** The entries of a record of `used` bytes, or null when one overruns it, has an illegal key, or a key is there twice. */
function entriesOf(b: Buffer, used: number): SettingsEntry[] | null {
  const out: SettingsEntry[] = [];
  let at = HEADER;
  while (at < used) {
    const keyLen = b[at]!;
    const ks = at + 1;
    if (keyLen === 0 || keyLen > SETTINGS_KEY_MAX || ks + keyLen + 2 > used) return null;
    const key = b.subarray(ks, ks + keyLen);
    const valueLen = b.readUInt16BE(ks + keyLen);
    const vs = ks + keyLen + 2;
    if (valueLen > SETTINGS_VALUE_MAX || vs + valueLen > used || !validKey(key)) return null;
    const name = key.toString('latin1');
    if (out.some(([k]) => k === name)) return null;
    out.push([name, Buffer.from(b.subarray(vs, vs + valueLen))]);
    at = vs + valueLen;
  }
  return at === used ? out : null;
}

/** `'ok'`, or the first rule the record breaks: magic, version, used, crc, entry. */
export function settingsWhy(b: Buffer): string {
  if (b.length < HEADER || b.subarray(0, 4).toString('latin1') !== 'DKST') return 'magic';
  if (b.readUInt16BE(4) !== 1) return 'version';
  const used = b.readUInt32BE(10);
  if (used < HEADER || used > SETTINGS_AREA || used > b.length) return 'used';
  if (b.readUInt32BE(14) !== crcOf(b, used)) return 'crc';
  return entriesOf(b, used) ? 'ok' : 'entry';
}

/** The record, or null when it is not a valid one. */
export function decodeSettings(b: Buffer): SettingsRecord | null {
  if (settingsWhy(b) !== 'ok') return null;
  const used = b.readUInt32BE(10);
  return { seq: b.readUInt32BE(6), used, entries: entriesOf(b, used)! };
}

/** A record of the entries in order (the runtime appends a new key at the end and keeps the order of the others). */
export function encodeSettings(seq: number, entries: SettingsEntry[]): Buffer {
  const body = Buffer.concat(
    entries.map(([k, v]) => {
      const len = Buffer.alloc(2);
      len.writeUInt16BE(v.length);
      return Buffer.concat([Buffer.from([k.length]), Buffer.from(k, 'latin1'), len, v]);
    }),
  );
  const head = Buffer.alloc(14);
  head.write('DKST', 0, 'latin1');
  head.writeUInt16BE(1, 4);
  head.writeUInt32BE(seq, 6);
  head.writeUInt32BE(HEADER + body.length, 10);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(zlib.crc32(Buffer.concat([head, body])));
  return Buffer.concat([head, crc, body]);
}

export const settingsMap = (rec: SettingsRecord | null) =>
  Object.fromEntries((rec?.entries ?? []).map(([k, v]) => [k, v.toString('hex')]));

// ---- settings through the mailbox and over USB ----------------------------------------------------------------------------------

const CMD_SETTINGS = { PUT: 9, GET: 10, REMOVE: 11 };
const MB_ARG = 12;
export const E_SET_KEY = -66;
export const E_SET_NOKEY = -68;
export const E_SET_OFF = -70;

/** The mailbox test commands CMD_SETTINGS_PUT, _GET and _REMOVE (argument: u8 key_len, key, u8 value_len, value). */
export function settingsMailbox(session: Session, plan: RuntimePlan) {
  const arg = (key: string, value: Buffer = Buffer.alloc(0)) =>
    Buffer.concat([Buffer.from([key.length]), Buffer.from(key), Buffer.from([value.length]), value]);
  return {
    put: (key: string, value: Buffer) => postRaw(session, plan, CMD_SETTINGS.PUT, arg(key, value)),
    remove: (key: string) => postRaw(session, plan, CMD_SETTINGS.REMOVE, arg(key)),
    /** (the result, the value): the runtime answers the value's length and leaves the bytes at the start of the mailbox argument. */
    async get(key: string): Promise<[number, Buffer]> {
      const n = await postRaw(session, plan, CMD_SETTINGS.GET, arg(key));
      if (n <= 0) return [n, Buffer.alloc(0)];
      const mb = planSymbol(plan, 'DKRT_MAILBOX');
      const bytes = Buffer.alloc((n + 3) & ~3);
      for (let i = 0; i < bytes.length; i += 4) bytes.writeUInt32BE(await word(session, mb + MB_ARG + i), i);
      return [n, bytes.subarray(0, n)];
    },
  };
}

/** SETTINGS (read only) in pieces of 768 bytes: the flags (bit 0: changes the card does not hold yet), the card's current copy (0 A, 1 B, 2 none), its seq and the record bytes. Throws a DkError with the device status. */
export async function settingsRecord(dk: Pick<DkSession, 'request'>) {
  let raw = Buffer.alloc(0);
  let total: number | null = null;
  let head = { flags: 0, copy: 2, seq: 0 };
  while (total === null || raw.length < total) {
    const body = Buffer.alloc(6);
    body.writeUInt32BE(raw.length, 0);
    body.writeUInt16BE(READ_MAX, 4);
    const reply = await dk.request(CMD.SETTINGS, body);
    if (reply.status !== 0) throw deviceError('SETTINGS', reply.status, reply.body);
    const b = Buffer.from(reply.body);
    if (b.length <= 10) throw new Error('SETTINGS: malformed or empty reply');
    const piece = { flags: b[0]!, copy: b[1]!, seq: b.readUInt32BE(2) };
    total ??= b.readUInt32BE(6);
    if (raw.length === 0) head = piece;
    else expect(piece, 'the record did not change while it was read').toEqual(head);
    raw = Buffer.concat([raw, b.subarray(10)]);
  }
  return { ...head, raw };
}

/** One sector of a card file, zero padded past its end. */
export const sectorOf = (card: string, sector: number) => readSectors(card, sector, 1);

// ---- the panel ---------------------------------------------------------------------------------------------------------------------

/** Turn an encoder (Python `do_turn` + `run_ms`). */
export async function panelTurn(session: Session, enc: string, by: number, ms: number) {
  await bounded(() => session.client.inputTurn(encoder(enc), by));
  await advance(session, 10 + ms);
}

/** Hold or release a key (Python `emu.button`). */
export const panelKey = (session: Session, key: string, down: boolean) =>
  bounded(() => session.client.inputButton(button(key), down));

// ---- the machine-type assignment ------------------------------------------------------------------------------------------------------

/** The 24-bit hash of a run-time machine's global id `<module id>:<machine id>`: the low 24 bits of FNV-1a 32. */
export function machineHash(module: string, machine: string): number {
  let h = 0x811c9dc5;
  for (const b of Buffer.from(`${module}:${machine}`)) h = Math.imul(h ^ b, 0x01000193) >>> 0;
  return h & 0xffffff;
}

/** The type number the runtime assigned to `<module>:<machine>` in this boot (`DKRT_ASSIGN`: 3 header words, 8 map words, then 64 module indices, 64 keys, 64 types). */
export async function dynType(session: Session, plan: RuntimePlan, module: string, machine: string): Promise<number> {
  const base = planSymbol(plan, 'DKRT_ASSIGN');
  const count = await word(session, base + 4);
  const want = machineHash(module, machine);
  for (let i = 0; i < count; i++) {
    const key = await word(session, base + 4 * (75 + i));
    const type = await word(session, base + 4 * (139 + i));
    if (key >>> 8 === want && type) return type;
  }
  throw new Error(`${module}:${machine} has no assigned type`);
}

// ---- the project slot (roundtrip) ----------------------------------------------------------------------------------------------------

export const SLOT0 = 0x58000;
const SLOT_SECTORS = 0x8000;
const SECTOR = 512;
/** soundStorage_v3: magic, version 3. */
const SOUND_HEAD = Buffer.from('beefbace00000003', 'hex');
/** The one word of the slot that the first save of a project sets differently from every later save (finding 39): offset 0x18, 4 bytes. */
export const STAMP: [number, number][] = [[0x18, 4]];
/** The machine type byte of a sound record (DT2 0xd2, DN2 0xf4). */
export const typeAt = (device: Device) => (device === 'dt2' ? 0xd2 : 0xf4);
/** Word of DT2 track 1 whose third byte is the machine type; the tracks lie 0x450 apart (finding 02). */
export const TRACK_OBJ_WORD = 0x4265338c;

/** The project-slot sectors of an overlay file, with the bytes the guest wrote laid over zeros. */
export function slotSectors(overlayFile: string): Map<number, Buffer> {
  const out = new Map<number, Buffer>();
  for (const { sector, mask, data } of overlayRecords(fs.readFileSync(overlayFile))) {
    if (sector < SLOT0 || sector >= SLOT0 + 128 * SLOT_SECTORS) continue;
    const buf = Buffer.alloc(SECTOR);
    for (let i = 0; i < SECTOR; i++) if (mask[i >> 3]! & (1 << (i & 7))) buf[i] = data[i]!;
    out.set(sector, buf);
  }
  return out;
}

export const slotImage = (card: string, touched: Iterable<number>) =>
  new Map([...touched].sort((a, b) => a - b).map((s) => [s, readSectors(card, s, 1)] as const));

/** (byte offset of the record, machine type) for each sound record in the written sectors (runs of adjacent sectors are joined). */
export function soundRecords(blob: Map<number, Buffer>, at: number): [number, number][] {
  const out: [number, number][] = [];
  let run: number[] = [];
  const flush = () => {
    const data = Buffer.concat(run.map((k) => blob.get(k)!));
    for (let i = data.indexOf(SOUND_HEAD); i >= 0; i = data.indexOf(SOUND_HEAD, i + 1))
      if (i + at < data.length) out.push([run[0]! * SECTOR + i, data[i + at]!]);
  };
  for (const s of [...blob.keys()].sort((a, b) => a - b)) {
    if (run.length && s !== run.at(-1)! + 1) {
      flush();
      run = [];
    }
    run.push(s);
  }
  if (run.length) flush();
  return out;
}

/** The part of the project slot a byte offset (from the slot start) falls in (finding 02). */
function regionOf(offset: number, device: Device): string {
  const kit = 0x200 + 128 * 0x15c00;
  const pool = device === 'dt2' ? 0xda5a04 : 0xc4b004 - 130 * 0x167;
  return offset < 0x200 ? 'header' : offset < kit ? 'pattern' : offset < pool ? 'kit' : 'pool';
}

/** Compare two slot images sector by sector: the sectors only one has and the bytes that differ, by region. `ignore`: byte ranges of the first slot that are left out. */
export function diffSlot(
  a: Map<number, Buffer>,
  b: Map<number, Buffer>,
  device: Device,
  ignore: [number, number][] = [],
) {
  const onlyFirst = [...a.keys()].filter((s) => !b.has(s));
  const onlySecond = [...b.keys()].filter((s) => !a.has(s));
  const skip = new Set(ignore.flatMap(([off, n]) => Array.from({ length: n }, (_, i) => SLOT0 * SECTOR + off + i)));
  const regions: Record<string, number> = {};
  for (const s of [...a.keys()].filter((k) => b.has(k)).sort((x, y) => x - y)) {
    const x = a.get(s)!;
    const y = b.get(s)!;
    let n = 0;
    for (let i = 0; i < SECTOR; i++) if (x[i] !== y[i] && !skip.has(s * SECTOR + i)) n++;
    if (n) {
      const region = regionOf(((s - SLOT0) % SLOT_SECTORS) * SECTOR, device);
      regions[region] = (regions[region] ?? 0) + n;
    }
  }
  return {
    onlyFirst: onlyFirst.length,
    onlySecond: onlySecond.length,
    differBytes: regions,
    equal: !onlyFirst.length && !onlySecond.length && !Object.keys(regions).length,
  };
}

/** (offset in pattern 0, byte of A, byte of B) for the bytes that differ in the first pattern record of slot 0 (16 tracks of trig, p-lock and sound-lock data). */
export function pattern0Changes(
  a: Map<number, Buffer>,
  b: Map<number, Buffer>,
  n = 0x15c00,
): [number, number, number][] {
  const out: [number, number, number][] = [];
  for (let k = 0; k < Math.floor(n / SECTOR) + 1; k++) {
    const sec = SLOT0 + Math.floor((0x200 + k * SECTOR) / SECTOR);
    const x = a.get(sec) ?? Buffer.alloc(SECTOR);
    const y = b.get(sec) ?? Buffer.alloc(SECTOR);
    for (let i = 0; i < SECTOR; i++)
      if (x[i] !== y[i]) out.push([sec * SECTOR - SLOT0 * SECTOR + i - 0x200, x[i]!, y[i]!]);
  }
  return out.filter((o) => o[0] < n);
}

/** The machine type byte of a DT2 track's sound. */
export async function trackType(session: Session, track: number): Promise<number> {
  return ((await word(session, TRACK_OBJ_WORD + 0x450 * (track - 1))) >>> 8) & 0xff;
}

// ---- tables and PCM (equiv) ------------------------------------------------------------------------------------------------------------

/** The NUL-terminated string at `addr`, read by words. */
async function cstr(session: Session, addr: number, limit = 40): Promise<string> {
  if (!addr) return '';
  const out: number[] = [];
  for (let k = 0; k < limit; k += 4) {
    const w = await word(session, addr + k);
    out.push(w >>> 24, (w >>> 16) & 255, (w >>> 8) & 255, w & 255);
    if (out.includes(0)) break;
  }
  const end = out.indexOf(0);
  return Buffer.from(end < 0 ? out : out.slice(0, end)).toString('latin1');
}

export interface Tables {
  machines: Record<string, unknown>;
  params: Record<string, unknown>;
  pages: { name: string; title: string; machine: string; knobs: (string | number)[]; level: number; slots: number[] }[];
}

/** The machine, parameter and page rows with a name, decoded without pointers or type numbers (parameters are named, knobs name their parameter, pages name their machine). */
export async function decodeTables(session: Session, plan: RuntimePlan): Promise<Tables> {
  const w = (a: number) => word(session, a);
  const sym = (name: string) => planSymbol(plan, name);
  // `*.count`, `*.first` and `machines.row_size` are constant symbols: their value is the number.
  const rows = sym('machines.rows');
  const count = sym('machines.count');
  const first = sym('machines.first');
  const size = sym('machines.row_size');
  const pf = sym('pf_table');
  const pfirst = sym('params.first');
  const pcount = sym('params.count');
  const pnames = new Map<number, string>();
  const params: Record<string, unknown> = {};
  for (let k = 0; k < pcount; k++) {
    const row = pf + 60 * (pfirst + k);
    const name = await cstr(session, await w(row + 40));
    if (!name) continue;
    pnames.set(pfirst + k, name);
    const words = await Promise.all(Array.from({ length: 15 }, (_, i) => w(row + 4 * i)));
    params[name] = {
      short: await cstr(session, words[12]!),
      mirror: words[1],
      min: words[2],
      max: words[3],
      default: words[4],
      template_words: [0, 5, 6, 7, 8, 9, 11, 13, 14].map((i) => words[i]),
    };
  }
  const knob = (c: number) => pnames.get(c) ?? c;
  const mnames = new Map<number, string>();
  const machines: Record<string, unknown> = {};
  for (let k = 0; k < count; k++) {
    const row = rows + size * k;
    const name = await cstr(session, await w(row + 56));
    if (!name) continue;
    mnames.set(first + k, name);
    const f = await Promise.all(Array.from({ length: 9 }, (_, i) => w(row + 8 + 4 * i)));
    machines[name] = {
      short: await cstr(session, await w(row + 60)),
      clone_of: await w(row + 44),
      own_dsp: await w(row + 52),
      knobs: f.slice(0, 8).map(knob),
      level: f[8],
    };
  }
  const pages: Tables['pages'] = [];
  let pageRows = 0;
  let pageCount = 0;
  try {
    pageRows = sym('pages.rows');
    pageCount = sym('pages.count');
  } catch {
    // the Digitakt II has no page rows
  }
  for (let k = 0; k < pageCount; k++) {
    const row = pageRows + 84 * k;
    const machine = await w(row + 44);
    const name = await cstr(session, await w(row));
    if (!name || !mnames.has(machine)) continue;
    pages.push({
      name,
      title: await cstr(session, await w(row + 4)),
      machine: mnames.get(machine)!,
      knobs: await Promise.all(Array.from({ length: 8 }, (_, i) => w(row + 8 + 4 * i).then(knob))),
      level: await w(row + 8 + 32),
      slots: await Promise.all(Array.from({ length: 9 }, (_, i) => w(row + 48 + 4 * i))),
    });
  }
  return { machines, params, pages };
}

/** Differences between two decoded table sets (empty: equal). */
export function diffTables(a: Tables, b: Tables): string[] {
  const out: string[] = [];
  for (const kind of ['machines', 'params'] as const)
    for (const name of [...new Set([...Object.keys(a[kind]), ...Object.keys(b[kind])])].sort())
      if (JSON.stringify(a[kind][name]) !== JSON.stringify(b[kind][name]))
        out.push(`${kind.slice(0, -1)} ${name}: ${JSON.stringify(a[kind][name])} != ${JSON.stringify(b[kind][name])}`);
  const key = (p: Tables['pages'][number]) => `${p.machine}\0${p.name}`;
  const sorted = (t: Tables) => [...t.pages].sort((x, y) => (key(x) < key(y) ? -1 : key(x) > key(y) ? 1 : 0));
  if (JSON.stringify(sorted(a)) !== JSON.stringify(sorted(b)))
    out.push(`pages: ${JSON.stringify(sorted(a))} != ${JSON.stringify(sorted(b))}`);
  return out;
}

/** The (type, pc) words of the eight voice rows of the DSP registry; a row is 24 words, `first` comes from the dyndsp info (word 6). */
export async function voiceRows(session: Session, plan: RuntimePlan): Promise<[number, number][]> {
  const rows = planSymbol(plan, 'voices.rows');
  const first = await word(session, planSymbol(plan, 'dyndsp_info') + 4 * 6);
  const out: [number, number][] = [];
  for (let k = 0; k < 8; k++) {
    const raw = await dspBytes(session, rows + 96 * (first + k), 8);
    out.push([raw.readUInt32LE(0), raw.readUInt32LE(4)]);
  }
  return out;
}
