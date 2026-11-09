// The fault record the runtime keeps on the card as the settings key `dk/fault` (core/runtime sdk `fault.rs`): reading the settings record over
// SETTINGS, decoding the 40 bytes, and the ABOUT rows of the menu. The same rows are in `dkrt_sdk::fault::rows` and `tools/device/dklog.py`.
import { CMD, READ_MAX } from './gen_proto.ts';
import { deviceError, DkError, type DkSession } from './dkupload-core.ts';

/** The settings key of the last fault. */
export const FAULT_KEY = 'dk/fault';

/** The value of `key` in a settings record (docs/design/digikit-store.md: `DKST`, u16 version, u32 seq, u32 used, u32 crc, then
 * `u8 key_len | key | u16 value_len | value` entries), or null. Not validated beyond the bounds: the runtime and `dksettings` check the CRC. */
export function settingsValue(record: Uint8Array, key: string): Uint8Array | null {
  const view = new DataView(record.buffer, record.byteOffset, record.byteLength);
  if (record.length < 18 || String.fromCharCode(...record.subarray(0, 4)) !== 'DKST') return null;
  const used = Math.min(view.getUint32(10), record.length);
  let at = 18;
  while (at < used) {
    const keyLen = record[at]!;
    const keyEnd = at + 1 + keyLen;
    if (keyLen === 0 || keyEnd + 2 > used) return null;
    const valueLen = view.getUint16(keyEnd);
    if (keyEnd + 2 + valueLen > used) return null;
    if (String.fromCharCode(...record.subarray(at + 1, keyEnd)) === key) {
      return record.subarray(keyEnd + 2, keyEnd + 2 + valueLen);
    }
    at = keyEnd + 2 + valueLen;
  }
  return null;
}

/** The settings record the runtime holds (SETTINGS, read in pieces of 768 bytes). */
export async function readSettingsRecord(dk: Pick<DkSession, 'request'>): Promise<Uint8Array> {
  const out: number[] = [];
  let total: number | null = null;
  while (total === null || out.length < total) {
    const body = new Uint8Array(6);
    const view = new DataView(body.buffer);
    view.setUint32(0, out.length);
    view.setUint16(4, READ_MAX);
    const reply = await dk.request(CMD.SETTINGS, body);
    if (reply.status !== 0) throw deviceError('SETTINGS', reply.status, reply.body);
    if (reply.body.length <= 10) throw new Error('SETTINGS: short record reply');
    const head = new DataView(reply.body.buffer, reply.body.byteOffset, reply.body.byteLength);
    total ??= head.getUint32(6);
    out.push(...reply.body.subarray(10));
  }
  return Uint8Array.from(out);
}

/** A pristine store may have no settings record. Only the explicit device status denotes absence. */
export async function readSettingsRecordIfPresent(dk: Pick<DkSession, 'request'>): Promise<Uint8Array | null> {
  try {
    return await readSettingsRecord(dk);
  } catch (error) {
    if (error instanceof DkError && error.kind === 'device' && error.status === 'E_SET_NOKEY') return null;
    throw error;
  }
}

export interface Fault {
  kind: 'cpu' | 'dsp' | 'panic';
  /** 0 unknown, 1 voice, 2 effect master, 3 effect send. */
  site: number;
  /** The core step: 1 ENTER, 4 CALL, 5 RET. */
  stage: number;
  id: number;
  /** The module's own step (0: none). */
  step: number;
  /** The PC of a CPU crash, or the DSP site's last word. */
  at: number;
  runtime: number;
  module: string;
}

/** The 40 bytes of `dk/fault`: `FLT1`, kind | site << 8 | stage << 16, id, module step, PC or last word, runtime, name. */
export function decodeFault(value: Uint8Array): Fault | null {
  if (value.length < 40) return null;
  const view = new DataView(value.buffer, value.byteOffset, value.byteLength);
  if (view.getUint32(0) !== 0x464c5431) return null;
  const k = view.getUint32(4);
  const kind = k & 0xff;
  if (kind !== 1 && kind !== 2 && kind !== 3) return null;
  const name = String.fromCharCode(...value.subarray(24, 40)).split('\0')[0]!;
  return {
    kind: kind === 1 ? 'cpu' : kind === 2 ? 'dsp' : 'panic',
    site: (k >> 8) & 0xff,
    stage: (k >> 16) & 0xff,
    id: view.getUint32(8),
    step: view.getUint32(12),
    at: view.getUint32(16),
    runtime: view.getUint32(20),
    module: name,
  };
}

/** The fault the runtime keeps, or null when there is none (or the runtime has no settings store). */
export async function readFault(dk: Pick<DkSession, 'request'>): Promise<Fault | null> {
  const value = settingsValue(await readSettingsRecord(dk), FAULT_KEY);
  return value ? decodeFault(value) : null;
}

const ROW = 18;
const SITES = ['SITE UNKNOWN', 'VOICE', 'FX MASTER', 'FX SEND'];
const CORE_STEPS: Record<number, string> = { 1: 'ENTER', 4: 'CALL', 5: 'RET' };

/** The ABOUT rows of a fault, as the runtime's menu shows them (at most 18 characters each): `FAULT: …`, `IN <MODULE>`, and the place. */
export function faultRows(f: Fault): string[] {
  const head = f.kind === 'cpu' ? 'FAULT: CPU CRASH' : f.kind === 'panic' ? 'FAULT: CPU PANIC' : 'FAULT: DSP STOP';
  const name = f.module
    .slice(f.module.lastIndexOf('/') + 1)
    .toUpperCase()
    .slice(0, ROW - 3);
  const rows = [head, name ? `IN ${name}` : f.kind === 'cpu' ? 'IN FIRMWARE' : 'IN UNKNOWN'];
  if (f.kind === 'cpu') rows.push(`PC ${f.at.toString(16).toUpperCase().padStart(8, '0')}`);
  else if (f.kind === 'dsp') {
    const site = SITES[f.site] ?? SITES[0]!;
    rows.push(f.site === 0 ? site : `${site} ${f.step ? `STEP ${f.step}` : (CORE_STEPS[f.stage] ?? '?')}`);
  }
  return rows.map((r) => r.slice(0, ROW));
}

/** One line for the Log: `DSP STOP in DYN-FX-HANG, FX SEND STEP 3` (the rows with the first two joined by "in"). */
export function faultSummary(f: Fault): string {
  const [head = '', who = '', ...place] = faultRows(f);
  return [`${head.replace(/^FAULT: /, '')} ${who.replace(/^IN /, 'in ')}`, ...place].join(', ');
}

/** Two records are the same fault (the card keeps one; a new one replaces it). */
export const sameFault = (a: Fault | null, b: Fault | null): boolean =>
  a === b || (!!a && !!b && JSON.stringify(a) === JSON.stringify(b));
