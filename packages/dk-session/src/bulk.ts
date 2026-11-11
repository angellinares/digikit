// The host side of the USB bulk link (docs/plans/usb-fast.md sections 5 and 7), beside the SysEx-shaped `DkTransport`, not instead of it.
//
// A `BulkLink` moves whole frames: the desktop app's Tauri link, the WebUSB link of the web app, the `dkbulk` bridge of the command line and the fake
// device of the tests implement it. `BulkSession` speaks the protocol over it (numbers in gen_bulk.ts, from sdk/sdk.toml [protocol.bulk]):
//   request: 'DKB1' | cmd u8 | seq u8 | flags u16 | len u32 | body      reply: 'DKB1' | cmd|0x40 | seq | status i16 | len u32 | body   (big-endian)
// Two DATA requests are in flight. The device takes the next offset only, so after a refused DATA (E_BUSY) the one behind it is refused too (E_OFFSET)
// and wrote nothing: the next request goes out at the first byte not acknowledged. The session keeps the sequence numbers of the requests whose reply it
// has not read: a reply that is not the one awaited is dropped and logged (never an error), and after a timeout or a failure the owed replies are read or
// discarded before the next request. Written without TS-only runtime syntax so node can import it.
import { DkError, be32, concat } from './dkupload-core.ts';
import {
  BULK_ALIGN,
  BULK_CMD,
  BULK_DATA_MAX,
  BULK_FLAG,
  BULK_FRAME_SLACK,
  BULK_HEADER,
  BULK_MAGIC,
  BULK_NONCE_LEN,
  BULK_PACKET,
  BULK_REPLY_FLAG,
  BULK_STATUS,
} from './gen_bulk.ts';

/**
 * `send` writes one whole request frame as USB transfers: when its length is a multiple of the packet size it is followed by a zero-length transfer
 * (`sendWithZlp`), because the device completes a multi-page RX buffer only when it is full or on a short packet. `recv` returns whole reply frames; a
 * zero-length IN transfer (the device keeps ZLT on EP2 IN) is "no bytes", never an end or an error.
 */
export interface BulkLink {
  /** Send one whole request frame; resolves when the transfers have completed. */
  send(frame: Uint8Array): Promise<void>;
  /** The next whole reply frame, or null after `timeoutMs` or when `signal` aborts. */
  recv(timeoutMs: number, signal?: AbortSignal): Promise<Uint8Array | null>;
  close(): Promise<void> | void;
  /** True once the link has ended (closed by us or lost: the device was unplugged or the interface went away). Optional: a link without it is judged by a request. */
  readonly closed?: boolean;
}

/** The ZLP rule: `out` is one OUT transfer. */
export async function sendWithZlp(out: (data: Uint8Array) => Promise<unknown>, frame: Uint8Array): Promise<void> {
  await out(frame);
  if (frame.length > 0 && frame.length % BULK_PACKET === 0) await out(new Uint8Array());
}

const magic = new TextEncoder().encode(BULK_MAGIC);
const text = new TextEncoder();
/** The largest frame: a DATA request with its header and the slack for BEGIN's path and the like. */
export const BULK_MAX_FRAME = BULK_DATA_MAX + 4 + BULK_HEADER + BULK_FRAME_SLACK;

export interface BulkReply {
  cmd: number;
  seq: number;
  status: number;
  body: Uint8Array;
}

/** One request frame. */
export function bulkRequest(cmd: number, seq: number, body: Uint8Array = new Uint8Array(), flags = 0): Uint8Array {
  const h = new Uint8Array(BULK_HEADER);
  h.set(magic);
  const v = new DataView(h.buffer);
  h[4] = cmd;
  h[5] = seq;
  v.setUint16(6, flags);
  v.setUint32(8, body.length);
  return concat(h, body);
}

/** One reply frame (the device side: fakes and tests). */
export function bulkReply(cmd: number, seq: number, status: number, body: Uint8Array = new Uint8Array()): Uint8Array {
  const h = new Uint8Array(BULK_HEADER);
  h.set(magic);
  const v = new DataView(h.buffer);
  h[4] = cmd | BULK_REPLY_FLAG;
  h[5] = seq;
  v.setInt16(6, status);
  v.setUint32(8, body.length);
  return concat(h, body);
}

/** A corrupt stream (bad magic, a length over the bound): fatal. */
export class BulkFrameError extends DkError {
  constructor(what: string) {
    super({ error: `bulk: ${what}`, kind: 'protocol' });
  }
}

/** The length of the frame that starts `b`: null when the header is not complete yet; throws `BulkFrameError` when it is not a frame. */
export function bulkFrameLength(b: Uint8Array): number | null {
  if (b.length < BULK_HEADER) return null;
  for (let i = 0; i < 4; i++) if (b[i] !== magic[i]) throw new BulkFrameError('bad frame magic');
  const n = BULK_HEADER + new DataView(b.buffer, b.byteOffset + 8, 4).getUint32(0);
  if (n > BULK_MAX_FRAME) throw new BulkFrameError(`a frame of ${n} bytes is over the bound ${BULK_MAX_FRAME}`);
  return n;
}

export function parseBulkReply(f: Uint8Array): BulkReply {
  const total = bulkFrameLength(f);
  if (total === null || f.length < total) throw new BulkFrameError('short reply frame');
  if (!(f[4]! & BULK_REPLY_FLAG)) throw new BulkFrameError('a request where a reply was expected');
  return {
    cmd: f[4]! & ~BULK_REPLY_FLAG,
    seq: f[5]!,
    status: new DataView(f.buffer, f.byteOffset + 6, 2).getInt16(0),
    body: f.subarray(BULK_HEADER, total),
  };
}

/** A request as the device sees it (the fake device and the tests). */
export function parseBulkRequest(f: Uint8Array): { cmd: number; seq: number; flags: number; body: Uint8Array } {
  const total = bulkFrameLength(f);
  if (total === null || f.length < total) throw new BulkFrameError('short request frame');
  return {
    cmd: f[4]!,
    seq: f[5]!,
    flags: new DataView(f.buffer, f.byteOffset + 6, 2).getUint16(0),
    body: f.subarray(BULK_HEADER, total),
  };
}

/** Frames out of a byte stream (WebUSB packets, the bridge's stdout): chunks may split or join frames, an empty chunk adds nothing. */
export class BulkFrames {
  private rx: Uint8Array = new Uint8Array();
  /** Throws `BulkFrameError` for a corrupt stream; the stream is not used after that. */
  feed(chunk: Uint8Array): Uint8Array[] {
    if (chunk.length === 0) return [];
    this.rx = this.rx.length ? concat(this.rx, chunk) : chunk;
    const out: Uint8Array[] = [];
    for (;;) {
      const n = bulkFrameLength(this.rx);
      if (n === null || this.rx.length < n) break;
      out.push(this.rx.slice(0, n));
      this.rx = this.rx.subarray(n);
    }
    if (this.rx.length && this.rx.buffer.byteLength > this.rx.length * 4) this.rx = this.rx.slice();
    return out;
  }
}

const STATUS_NAMES = new Map<number, string>(Object.entries(BULK_STATUS).map(([n, c]) => [c, n]));
export const E_BUSY = BULK_STATUS.L_E_BUSY;
export const E_EXISTS = BULK_STATUS.L_E_EXISTS;

/** A device status other than 0 (`status` is the name: `E_HASH`, `L_E_EXISTS`). */
export class BulkDeviceError extends DkError {
  code: number;
  command: number;
  constructor(command: number, code: number) {
    const name = STATUS_NAMES.get(code) ?? `status ${code}`;
    super({
      error: `bulk command ${command.toString(16)}: the device answered ${name} (${code})`,
      kind: 'device',
      status: name,
    });
    this.code = code;
    this.command = command;
  }
}

export interface BulkOptions {
  /** Per reply. A DATA reply can take as long as the card needs for 64 KiB. */
  timeoutMs?: number;
  /** How long `resync` waits for each reply still owed. */
  resyncMs?: number;
  /** DATA requests in flight. */
  window?: number;
  /** Data bytes of a DATA request: a multiple of BULK_ALIGN, at most BULK_DATA_MAX. */
  chunk?: number;
  /** How often a request that got E_BUSY is sent again in a row (reset by every acknowledged DATA). */
  busyRetries?: number;
  busyWaitMs?: number;
  /** What the session did on its own (late replies dropped, resyncs). */
  log?: (line: string) => void;
}
export interface BulkInfo {
  version: number;
  flags: number;
  dataMax: number;
  align: number;
  /** What SET_NONCE left in the device (all zero before). */
  nonce: Uint8Array;
}
export interface BulkPutOptions {
  /** The file hash (`hash|1`, u32) the device must reach: sent in BEGIN, checked by the device at END and against what END returns. */
  expected?: number;
  onProgress?: (sent: number, total: number) => void;
  signal?: AbortSignal;
  /** The destination may exist: the device replaces it atomically at END (REPLACE flag of BEGIN); a failed upload keeps the old file. Clear: E_EXISTS. */
  replace?: boolean;
}

const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));
const cancelled = () => new DkError({ error: 'cancelled', kind: 'cancelled' });
const timedOut = (what: string) => new DkError({ error: `${what}: no reply`, kind: 'timeout' });

export class BulkSession {
  private link: BulkLink;
  private o: Required<Omit<BulkOptions, 'log'>> & { log: (line: string) => void };
  private seq = 0;
  /** Sequence numbers of the requests sent whose reply has not been read, oldest first. */
  private owed: number[] = [];
  private fatalError: Error | null = null;
  constructor(link: BulkLink, o: BulkOptions = {}) {
    this.link = link;
    this.o = {
      timeoutMs: o.timeoutMs ?? 5000,
      resyncMs: o.resyncMs ?? 500,
      window: o.window ?? 2,
      chunk: o.chunk ?? BULK_DATA_MAX,
      busyRetries: o.busyRetries ?? 150,
      busyWaitMs: o.busyWaitMs ?? 20,
      log: o.log ?? (() => {}),
    };
    if (this.o.chunk % BULK_ALIGN !== 0 || this.o.chunk < BULK_ALIGN || this.o.chunk > BULK_DATA_MAX)
      throw new DkError({
        error: `bulk chunk ${this.o.chunk}: a multiple of ${BULK_ALIGN} up to ${BULK_DATA_MAX}`,
        kind: 'usage',
      });
  }

  /** True once a protocol error ended the session (it closed the link). */
  get failed(): boolean {
    return this.fatalError !== null;
  }

  /** The number of requests whose reply is still owed (tests). */
  get outstanding(): number {
    return this.owed.length;
  }

  private async fatal(e: Error): Promise<never> {
    this.fatalError = e;
    await Promise.resolve(this.link.close()).catch(() => {});
    throw e;
  }
  private live() {
    if (this.fatalError) throw this.fatalError;
  }

  private async send(cmd: number, body?: Uint8Array, flags = 0): Promise<number> {
    this.live();
    const seq = this.seq;
    this.seq = (this.seq + 1) & 255;
    await this.link.send(bulkRequest(cmd, seq, body, flags));
    this.owed.push(seq);
    return seq;
  }

  /** The next frame, or null on a timeout; a cancel is an error of kind `cancelled`; a corrupt stream is fatal. */
  private async frame(waitMs: number, what: string, signal?: AbortSignal): Promise<Uint8Array> {
    this.live();
    if (signal?.aborted) throw cancelled();
    let f: Uint8Array | null;
    try {
      f = await this.link.recv(waitMs, signal);
    } catch (e) {
      if (e instanceof BulkFrameError) return this.fatal(e);
      throw e;
    }
    if (f) return f;
    if (signal?.aborted) throw cancelled();
    throw timedOut(what);
  }

  private settle(r: BulkReply) {
    const at = this.owed.indexOf(r.seq);
    if (at >= 0) this.owed.splice(0, at + 1);
  }

  /** The reply to request `want`; replies to other requests (late ones) are dropped and logged. */
  private async reply(cmd: number, want: number, signal?: AbortSignal): Promise<BulkReply> {
    const end = Date.now() + this.o.timeoutMs;
    for (;;) {
      // a stream of replies that never match is a timeout too
      const f = await this.frame(Math.max(1, end - Date.now()), `bulk command ${cmd.toString(16)}`, signal);
      let r: BulkReply;
      try {
        r = parseBulkReply(f);
      } catch (e) {
        return this.fatal(e as Error);
      }
      this.settle(r);
      if (r.seq === want && r.cmd === cmd) return r;
      this.o.log(`bulk: dropped a reply to ${r.cmd.toString(16)} seq ${r.seq} while waiting for seq ${want}`);
      if (Date.now() >= end) throw timedOut(`bulk command ${cmd.toString(16)}`);
    }
  }

  /** Read or discard the replies still owed; a reply that never comes ends the wait and the rest is forgotten. */
  private async resync(): Promise<void> {
    const end = Date.now() + this.o.resyncMs * (this.owed.length + 1);
    while (this.owed.length) {
      let f: Uint8Array;
      try {
        if (Date.now() >= end) throw timedOut('resync');
        f = await this.frame(this.o.resyncMs, 'resync');
      } catch (e) {
        if ((e as DkError).kind !== 'timeout') throw e;
        this.o.log(`bulk: resync: ${this.owed.length} replies never came`);
        this.owed.length = 0;
        return;
      }
      try {
        const r = parseBulkReply(f);
        this.o.log(`bulk: resync: late reply to ${r.cmd.toString(16)} seq ${r.seq}`);
        this.settle(r);
      } catch (e) {
        await this.fatal(e as Error);
      }
    }
  }

  /** One request and its reply, sent again after E_BUSY up to `retries` times. */
  private async callWith(
    cmd: number,
    body: Uint8Array | undefined,
    flags: number,
    retries: number,
  ): Promise<Uint8Array> {
    await this.resync();
    for (let tries = 0; ; tries++) {
      const r = await this.reply(cmd, await this.send(cmd, body, flags));
      if (r.status === 0) return r.body;
      if (r.status !== E_BUSY || tries >= retries) throw new BulkDeviceError(cmd, r.status);
      await sleep(this.o.busyWaitMs);
    }
  }
  private call(cmd: number, body?: Uint8Array, flags = 0): Promise<Uint8Array> {
    return this.callWith(cmd, body, flags, this.o.busyRetries);
  }

  async info(): Promise<BulkInfo> {
    const b = await this.call(BULK_CMD.INFO);
    if (b.length < 12 + BULK_NONCE_LEN)
      throw new DkError({ error: `bulk INFO body of ${b.length} bytes`, kind: 'protocol' });
    const v = new DataView(b.buffer, b.byteOffset, b.length);
    return {
      version: b[0]!,
      flags: b[1]!,
      dataMax: v.getUint32(4),
      align: v.getUint32(8),
      nonce: b.slice(12, 12 + BULK_NONCE_LEN),
    };
  }

  /** Drop the device's write session: the owed replies are read or discarded first, then ABORT, and its reply is confirmed. */
  abort(): Promise<unknown> {
    return this.call(BULK_CMD.ABORT);
  }

  /** The device's hash (u32) of `length` bytes at `offset` of the open or the last file. */
  async verify(offset: number, length: number): Promise<number> {
    return hashOf(await this.call(BULK_CMD.VERIFY, concat(be32(offset), be32(length))));
  }

  /**
   * Write a file. `replace`: the destination may exist and is replaced atomically at END, a failed upload keeps the old file; clear: an existing file
   * gives E_EXISTS. Returns the device's hash (u32) from END. A failure or a cancel aborts the device's session; a hash that END returns and is not
   * `expected` throws (the file stays: the caller deletes it over SysEx).
   */
  async put(root: number, path: string, bytes: Uint8Array, o: BulkPutOptions = {}): Promise<number> {
    const { expected, onProgress, signal } = o;
    const begin = concat(be32(root), text.encode(path), Uint8Array.of(0), be32(bytes.length), be32(expected ?? 0));
    const flags = o.replace ? BULK_FLAG.REPLACE : 0;
    try {
      await this.call(BULK_CMD.BEGIN, begin, flags);
    } catch (e) {
      if (!(e instanceof BulkDeviceError) || e.code !== E_BUSY) throw e;
      // still busy after the retries: a session of ours may be left open (an earlier upload that failed): drop it once, try once more
      this.o.log('bulk: BEGIN stayed busy: ABORT, then BEGIN once more');
      await this.abortSession().catch(() => {});
      await this.callWith(BULK_CMD.BEGIN, begin, flags, 0);
    }
    try {
      await this.transfer(bytes, onProgress, signal);
    } catch (e) {
      await this.abortSession().catch(() => {});
      throw e;
    }
    let hash: number;
    try {
      hash = hashOf(await this.call(BULK_CMD.END));
    } catch (e) {
      await this.abortSession().catch(() => {});
      throw e;
    }
    if (expected !== undefined && hash !== expected >>> 0)
      throw new DkError({
        error: `bulk: the device hash ${hash.toString(16)} is not the expected ${(expected >>> 0).toString(16)}`,
        kind: 'verify',
        status: 'E_HASH',
      });
    return hash;
  }

  private async abortSession(): Promise<void> {
    this.live();
    await this.abort();
  }

  /**
   * DATA requests with `window` in flight. A request leaves `owed` only when its reply is read, so an error here leaves every outstanding reply (the head
   * included) for the caller's `abort`.
   */
  private async transfer(bytes: Uint8Array, onProgress?: (sent: number, total: number) => void, signal?: AbortSignal) {
    const total = bytes.length;
    let next = 0;
    let acked = 0;
    let busy = false;
    let tries = 0;
    const inflight: { seq: number; len: number }[] = [];
    onProgress?.(0, total);
    for (;;) {
      if (signal?.aborted) throw cancelled();
      while (!busy && inflight.length < this.o.window && next < total) {
        const len = Math.min(this.o.chunk, total - next);
        const seq = await this.send(BULK_CMD.DATA, concat(be32(next), bytes.subarray(next, next + len)));
        inflight.push({ seq, len });
        next += len;
      }
      const head = inflight[0];
      if (!head) {
        if (!busy) return;
        if (++tries > this.o.busyRetries) throw new BulkDeviceError(BULK_CMD.DATA, E_BUSY);
        await sleep(this.o.busyWaitMs);
        busy = false;
        next = acked;
        continue;
      }
      const r = await this.reply(BULK_CMD.DATA, head.seq, signal);
      inflight.shift();
      if (r.status === 0 && !busy) {
        acked += head.len;
        tries = 0; // the budget is for a run of busy answers, not for the whole file
        onProgress?.(acked, total);
      } else if (r.status === E_BUSY) busy = true;
      else if (!busy) throw new BulkDeviceError(BULK_CMD.DATA, r.status);
      // else: behind a busy reply, refused and wrote nothing
    }
  }
}

function hashOf(body: Uint8Array): number {
  if (body.length < 4)
    throw new DkError({ error: `bulk: a reply of ${body.length} body bytes is not a hash`, kind: 'protocol' });
  return new DataView(body.buffer, body.byteOffset, 4).getUint32(0);
}
