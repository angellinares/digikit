// DKBS: 128x64 1-bit boot screen animation, provisional format v1: header, records, XOR-RUN codec (docs/design/dkbs.md).
// The decoder in C (the boot screen module's src/dkbs.c) reads the same bytes as this decoder. No dependencies; runs in Node and in the browser.

export const WIDTH = 128;
export const HEIGHT = 64;
export const FRAME_BYTES = 1024;
export const MAX_TICKS = 300;
export const MAX_FILE_BYTES = 65536;
export const TICKS_PER_SECOND = 30;

const HEADER_BYTES = 16;
const RECORD_BYTES = 8;
const CODEC_RAW = 0;
const CODEC_XOR_RUN = 1;

export interface Frame {
  bitmap: Uint8Array;
  ticks: number;
}

export interface DecodedAsset {
  ticks: number;
  frames: Frame[];
}

export class DkbsError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'DkbsError';
  }
}

const CRC_TABLE = (() => {
  const table = new Uint32Array(256);
  for (let n = 0; n < 256; n += 1) {
    let c = n;
    for (let k = 0; k < 8; k += 1) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    table[n] = c >>> 0;
  }
  return table;
})();

/** Same value as zlib.crc32. */
function crc32(data: Uint8Array): number {
  let c = 0xffffffff;
  for (let i = 0; i < data.length; i += 1) c = CRC_TABLE[(c ^ data[i]!) & 255]! ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

function integer(value: unknown, low: number, high: number, name: string): number {
  if (typeof value !== 'number' || !Number.isInteger(value) || value < low || value > high)
    throw new DkbsError(`${name} must be an integer in ${low}..${high}`);
  return value;
}

/** Run-length packing of an XOR frame: control 0..127 is a literal of ctrl+1 bytes, 128..255 repeats one byte (ctrl&127)+1 times. */
function packRuns(data: Uint8Array): Uint8Array {
  const out: number[] = [];
  let at = 0;
  while (at < data.length) {
    let end = at + 1;
    const limit = Math.min(at + 128, data.length);
    while (end < limit && data[end] === data[at]) end += 1;
    if (end - at >= 2) {
      out.push(0x80 | (end - at - 1), data[at]!);
    } else {
      while (end < limit) {
        if (end + 1 < data.length && data[end] === data[end + 1]) break;
        end += 1;
      }
      out.push(end - at - 1);
      for (let i = at; i < end; i += 1) out.push(data[i]!);
    }
    at = end;
  }
  return Uint8Array.from(out);
}

function unpackRuns(payload: Uint8Array): Uint8Array {
  const result = new Uint8Array(FRAME_BYTES);
  let length = 0;
  let at = 0;
  while (at < payload.length) {
    const ctrl = payload[at]!;
    const count = (ctrl & 127) + 1;
    at += 1;
    const take = ctrl & 128 ? 1 : count;
    if (length + count > FRAME_BYTES || at + take > payload.length)
      throw new DkbsError('XOR-RUN expansion exceeds frame or payload is truncated');
    if (ctrl & 128) result.fill(payload[at]!, length, length + count);
    else result.set(payload.subarray(at, at + count), length);
    length += count;
    at += take;
  }
  if (length !== FRAME_BYTES) throw new DkbsError('XOR-RUN must expand to exactly 1024 bytes');
  return result;
}

function xor(a: Uint8Array, b: Uint8Array): Uint8Array {
  const out = new Uint8Array(a.length);
  for (let i = 0; i < a.length; i += 1) out[i] = a[i]! ^ b[i]!;
  return out;
}

function same(a: Uint8Array, b: Uint8Array): boolean {
  for (let i = 0; i < a.length; i += 1) if (a[i] !== b[i]) return false;
  return true;
}

export function encode(frames: readonly Frame[]): Uint8Array {
  if (frames.length < 1 || frames.length > MAX_TICKS) throw new DkbsError('record count must be in 1..300');
  const merged: Frame[] = [];
  let total = 0;
  for (const frame of frames) {
    const ticks = integer(frame.ticks, 1, MAX_TICKS, 'ticks');
    if (!(frame.bitmap instanceof Uint8Array) || frame.bitmap.length !== FRAME_BYTES)
      throw new DkbsError('native frame must be exactly 1024 bytes');
    total += ticks;
    if (total > MAX_TICKS) throw new DkbsError('total ticks must be in 1..300');
    const last = merged[merged.length - 1];
    if (last && same(last.bitmap, frame.bitmap))
      merged[merged.length - 1] = { bitmap: frame.bitmap, ticks: last.ticks + ticks };
    else merged.push(frame);
  }
  const body: number[] = [];
  let previous: Uint8Array = new Uint8Array(FRAME_BYTES);
  for (const frame of merged) {
    const packed = packRuns(xor(frame.bitmap, previous));
    const [codec, payload] = packed.length < FRAME_BYTES ? [CODEC_XOR_RUN, packed] : [CODEC_RAW, frame.bitmap];
    body.push(frame.ticks >> 8, frame.ticks & 255, payload.length >> 8, payload.length & 255, codec, 0, 0, 0);
    for (let i = 0; i < payload.length; i += 1) body.push(payload[i]!);
    if (HEADER_BYTES + body.length > MAX_FILE_BYTES)
      throw new DkbsError(`asset needs at least ${HEADER_BYTES + body.length} bytes; maximum is ${MAX_FILE_BYTES}`);
    previous = frame.bitmap;
  }
  const bodyBytes = Uint8Array.from(body);
  const out = new Uint8Array(HEADER_BYTES + bodyBytes.length);
  const view = new DataView(out.buffer);
  out.set([0x44, 0x4b, 0x42, 0x53], 0); // "DKBS"
  view.setUint16(4, 1);
  view.setUint16(6, merged.length);
  view.setUint32(8, total);
  view.setUint32(12, crc32(bodyBytes));
  out.set(bodyBytes, HEADER_BYTES);
  return out;
}

export function decode(bytes: Uint8Array): DecodedAsset {
  if (bytes.length > MAX_FILE_BYTES)
    throw new DkbsError(`asset is ${bytes.length} bytes; maximum is ${MAX_FILE_BYTES}`);
  if (bytes.length < HEADER_BYTES) throw new DkbsError('truncated DKBS header');
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  const magic = String.fromCharCode(bytes[0]!, bytes[1]!, bytes[2]!, bytes[3]!);
  if (magic !== 'DKBS' || view.getUint16(4) !== 1) throw new DkbsError('unsupported DKBS magic/version');
  const count = integer(view.getUint16(6), 1, MAX_TICKS, 'record count');
  const ticks = integer(view.getUint32(8), 1, MAX_TICKS, 'total ticks');
  if (crc32(bytes.subarray(HEADER_BYTES)) !== view.getUint32(12)) throw new DkbsError('body CRC32 mismatch');
  let at = HEADER_BYTES;
  let total = 0;
  const frames: Frame[] = [];
  let previous: Uint8Array = new Uint8Array(FRAME_BYTES);
  for (let i = 0; i < count; i += 1) {
    if (at + RECORD_BYTES > bytes.length) throw new DkbsError('truncated record header');
    const hold = integer(view.getUint16(at), 1, MAX_TICKS, 'hold ticks');
    const size = view.getUint16(at + 2);
    const codec = bytes[at + 4]!;
    total += hold;
    if (total > ticks) throw new DkbsError('record holds exceed declared total ticks');
    if (
      bytes[at + 5] !== 0 ||
      bytes[at + 6] !== 0 ||
      bytes[at + 7] !== 0 ||
      (codec !== CODEC_RAW && codec !== CODEC_XOR_RUN)
    )
      throw new DkbsError('invalid codec or nonzero reserved bytes');
    if (size === 0 || (codec === CODEC_RAW && size !== FRAME_BYTES)) throw new DkbsError('invalid payload length');
    at += RECORD_BYTES;
    if (at + size > bytes.length) throw new DkbsError('truncated record payload');
    const payload = bytes.subarray(at, at + size);
    const bitmap = codec === CODEC_RAW ? payload.slice() : xor(unpackRuns(payload), previous);
    frames.push({ bitmap, ticks: hold });
    previous = bitmap;
    at += size;
  }
  if (at !== bytes.length || total !== ticks) throw new DkbsError('trailing bytes or mismatched total ticks');
  return { ticks, frames };
}

/** RGBA 128x64 to a native bitmap: composite over black, luma (77R+150G+29B+128)>>8, on when above the threshold. */
export function rgbaToBitmap(rgba: Uint8Array | Uint8ClampedArray, threshold = 128): Uint8Array {
  if (rgba.length !== WIDTH * HEIGHT * 4) throw new DkbsError('expected 128x64 RGBA data');
  integer(threshold, 0, 255, 'threshold');
  const bitmap = new Uint8Array(FRAME_BYTES);
  for (let y = 0; y < HEIGHT; y += 1)
    for (let x = 0; x < WIDTH; x += 1) {
      const at = 4 * (y * WIDTH + x);
      const a = rgba[at + 3]!;
      // over black: channel * alpha / 255, rounded as 8-bit alpha blending
      const r = Math.round((rgba[at]! * a) / 255);
      const g = Math.round((rgba[at + 1]! * a) / 255);
      const b = Math.round((rgba[at + 2]! * a) / 255);
      if ((77 * r + 150 * g + 29 * b + 128) >> 8 > threshold)
        bitmap[8 * x + ((63 - y) >> 3)]! |= 1 << (7 - ((63 - y) % 8));
    }
  return bitmap;
}

export function bitmapPixel(bitmap: Uint8Array, x: number, y: number): boolean {
  return (bitmap[8 * x + ((63 - y) >> 3)]! & (1 << (7 - ((63 - y) % 8)))) !== 0;
}
