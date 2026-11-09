// `DkSession.put` with several DATA requests in flight, against a fake device that keeps the runtime's DATA rule (the next offset, or the exact repeat of the last chunk).
import assert from 'node:assert/strict';
import { test } from 'vitest';
import { DkError, DkSession, decode8in7, encode8in7, fileHash } from '../src/index.ts';

const E_OFFSET = -54;
const E_WRITE = -57;
const CMD = { INFO: 0x01, BEGIN: 0x10, DATA: 0x11, END: 0x12, ABORT: 0x15 };
const concat = (...p: Uint8Array[]) => Uint8Array.from(Buffer.concat(p.map((x) => Buffer.from(x))));
const reply = (cmd: number, seq: number, status: number, body: Uint8Array = new Uint8Array()) =>
  concat(
    Uint8Array.of(0xf0, 0x7d, 0x64, 0x6b, 1, cmd | 0x40, seq),
    encode8in7(concat(Uint8Array.of(status & 255), body)),
    Uint8Array.of(0xf7),
  );
const u32 = (b: Uint8Array, at = 0) => new DataView(b.buffer, b.byteOffset + at, 4).getUint32(0);
const info = () => {
  const b = new Uint8Array(33);
  const v = new DataView(b.buffer);
  v.setUint16(0, 1);
  v.setUint16(2, 4);
  b.set(new TextEncoder().encode('dn2'), 4);
  v.setUint32(9, 1 << 30);
  v.setUint32(13, 32768);
  v.setUint32(29, 1 << 12); // capability FILEHASH
  return b;
};

class FakeDevice {
  readonly dataDepth: number | undefined;
  got: number[] = [];
  next = 0;
  lastOff = -1;
  lastLen = -1;
  size = 0;
  hash = new Uint8Array();
  open = false;
  ended: boolean | null = null;
  seen: string[] = [];
  dataSent = 0;
  outstanding = 0;
  maxOutstanding = 0;
  /** reply ordinals (counting DATA requests from 0) to drop, and offsets that answer an error */
  dropReply = new Set<number>();
  failAt = new Map<number, number>();
  /** Never deliver replies (a stalled link). */
  mute = false;
  private queue: Uint8Array[] = [];
  private waiter: (() => void) | null = null;
  constructor(depth?: number) {
    this.dataDepth = depth;
  }
  private push(m: Uint8Array) {
    this.queue.push(m);
    this.waiter?.();
  }
  send(msg: Uint8Array) {
    const cmd = msg[5]!;
    const seq = msg[6]!;
    const body = decode8in7(msg.subarray(7, msg.length - 1));
    this.seen.push(Object.entries(CMD).find(([, v]) => v === cmd)?.[0] ?? String(cmd));
    this.outstanding++;
    this.maxOutstanding = Math.max(this.maxOutstanding, this.outstanding);
    if (cmd === CMD.INFO) return this.out(cmd, seq, 0, info());
    if (cmd === CMD.BEGIN) {
      const z = body.indexOf(0);
      this.size = u32(body, z + 1);
      this.hash = body.slice(z + 5, z + 37);
      this.got = [];
      this.next = 0;
      this.lastOff = this.lastLen = -1;
      this.open = true;
      return this.out(cmd, seq, 0);
    }
    if (cmd === CMD.ABORT) {
      this.open = false;
      return this.out(cmd, seq, 0);
    }
    if (cmd === CMD.END) {
      const ok = this.next === this.size && fileHash(Uint8Array.from(this.got)).every((x, i) => x === this.hash[i]);
      this.ended = ok;
      this.open = false;
      return this.out(cmd, seq, ok ? 0 : -53);
    }
    assert.equal(cmd, CMD.DATA);
    const ordinal = this.dataSent++;
    const off = u32(body);
    const data = body.subarray(4);
    const err = this.failAt.get(off);
    let status = 0;
    if (!this.open) status = -55;
    else if (err !== undefined) status = err;
    else if (off === this.lastOff && data.length === this.lastLen) status = 0;
    else if (off !== this.next) status = E_OFFSET;
    else {
      this.got.push(...data);
      this.next = off + data.length;
      this.lastOff = off;
      this.lastLen = data.length;
    }
    if (this.dropReply.has(ordinal)) {
      this.outstanding--;
      return;
    }
    this.out(cmd, seq, status);
  }
  private out(cmd: number, seq: number, status: number, body?: Uint8Array) {
    if (!this.mute) this.push(reply(cmd, seq, status, body));
  }
  async recv(timeoutMs: number, signal?: AbortSignal): Promise<Uint8Array | null> {
    await new Promise((r) => setImmediate(r)); // a link takes a moment: the host's other work (timers) runs between replies
    if (this.queue.length === 0) {
      await new Promise<void>((resolve) => {
        const done = () => {
          clearTimeout(timer);
          this.waiter = null;
          resolve();
        };
        const timer = setTimeout(done, timeoutMs);
        this.waiter = done;
        signal?.addEventListener('abort', done, { once: true });
      });
    }
    const m = this.queue.shift() ?? null;
    if (m) this.outstanding--;
    return m;
  }
}

const bytes = (n: number) => Uint8Array.from({ length: n }, (_, i) => (i * 7 + (i >> 8)) & 255);
const CH = 512;
const session = (d: FakeDevice, depth?: number) => new DkSession(d, { timeoutMs: 60, retries: 1, chunk: CH, depth });
const progressOf = (log: number[]) => {
  for (let i = 1; i < log.length; i++) assert.ok(log[i]! >= log[i - 1]!, 'progress went back');
};

test('depth 2: two requests in flight, replies in order, the bytes arrive, progress never goes back', async () => {
  const d = new FakeDevice();
  const data = bytes(CH * 20 + 100);
  const log: number[] = [];
  await session(d, 2).put('a/b', data, (n, total) => (assert.equal(total, data.length), log.push(n)));
  assert.equal(d.maxOutstanding, 2);
  assert.equal(Buffer.compare(Buffer.from(d.got), Buffer.from(data)), 0);
  assert.equal(d.ended, true);
  assert.equal(d.dataSent, 21);
  assert.equal(log.length, 21);
  assert.equal(log.at(-1), data.length);
  progressOf(log);
});

test('the depth comes from the transport, and defaults to 1', async () => {
  const one = new FakeDevice();
  await new DkSession(one, { chunk: CH }).put('a', bytes(CH * 4));
  assert.equal(one.maxOutstanding, 1);
  const two = new FakeDevice(2);
  await new DkSession(two, { chunk: CH }).put('a', bytes(CH * 4));
  assert.equal(two.maxOutstanding, 2);
  const forced = new FakeDevice(2);
  await new DkSession(forced, { chunk: CH, depth: 1 }).put('a', bytes(CH * 4));
  assert.equal(forced.maxOutstanding, 1, 'the option wins');
});

test('files of 0 bytes, under two chunks, and on a chunk edge', async () => {
  for (const n of [0, 1, CH - 1, CH, CH + 1, 2 * CH, 3 * CH + 5, 8 * CH, 8 * CH + 1, 16 * CH]) {
    const d = new FakeDevice();
    const log: number[] = [];
    await session(d, 2).put('f', bytes(n), (sent) => log.push(sent));
    assert.equal(Buffer.compare(Buffer.from(d.got), Buffer.from(bytes(n))), 0, `size ${n}`);
    assert.equal(d.ended, true, `size ${n}`);
    assert.equal(d.dataSent, Math.ceil(n / CH));
    assert.equal(log.at(-1) ?? 0, n);
    progressOf(log);
  }
});

test('a lost reply with another request behind it: the later ok reply shows it was done, nothing is sent again', async () => {
  const d = new FakeDevice();
  d.dropReply.add(4);
  const data = bytes(CH * 12);
  const log: number[] = [];
  await session(d, 2).put('f', data, (n) => log.push(n));
  assert.equal(d.dataSent, 12);
  assert.equal(d.ended, true);
  assert.equal(log.length, 12);
  progressOf(log);
});

test('a lost reply with nothing behind it (the last chunk, and the last of a window): a timeout, the repeat is taken', async () => {
  for (const lost of [7, 11]) {
    const d = new FakeDevice();
    d.dropReply.add(lost);
    const data = bytes(CH * 12);
    const log: number[] = [];
    await session(d, 2).put('f', data, (n) => log.push(n));
    assert.equal(Buffer.compare(Buffer.from(d.got), Buffer.from(data)), 0);
    assert.equal(d.ended, true);
    assert.ok(d.dataSent > 12, 'the lost chunk was sent again');
    assert.equal(log.at(-1), data.length);
    progressOf(log);
  }
});

test('both replies of the window lost: the chunks are sent again from the first, E_OFFSET for the one that was done is fine', async () => {
  const d = new FakeDevice();
  d.dropReply.add(2).add(3);
  const data = bytes(CH * 6);
  const log: number[] = [];
  await session(d, 2).put('f', data, (n) => log.push(n));
  assert.equal(Buffer.compare(Buffer.from(d.got), Buffer.from(data)), 0);
  assert.equal(d.ended, true);
  assert.equal(log.length, 6);
  progressOf(log);
});

test('a device error mid-window stops the sending, ends the upload with the first error and aborts; the session is usable', async () => {
  for (const [status, name] of [
    [E_WRITE, 'E_WRITE'],
    [E_OFFSET, 'E_OFFSET'],
  ] as const) {
    const d = new FakeDevice();
    d.failAt.set(CH * 5, status);
    const s = session(d, 2);
    const log: number[] = [];
    await assert.rejects(
      s.put('f', bytes(CH * 20), (n) => log.push(n)),
      (e) => e instanceof DkError && e.kind === 'device' && e.status === name && /data at 2560/.test(e.message),
    );
    assert.ok(d.dataSent <= 7, `no new request after the error (sent ${d.dataSent})`);
    assert.equal(d.seen.at(-1), 'ABORT');
    assert.equal(d.open, false);
    assert.equal(log.at(-1), CH * 5, 'progress counts the acknowledged bytes only');
    assert.equal((await s.info()).port, 'dn2');
  }
});

test('cancelPending mid-window ends the put at once, aborts, and the session stays usable', async () => {
  const d = new FakeDevice();
  const s = new DkSession(d, { timeoutMs: 30_000, chunk: CH, depth: 2 });
  await s.info();
  d.mute = false;
  const p = s.put('f', bytes(CH * 2000));
  // let BEGIN through, then stop the replies: both DATA requests stay in flight
  await new Promise((r) => setTimeout(r, 10));
  d.mute = true;
  await new Promise((r) => setTimeout(r, 50));
  s.cancelPending();
  d.mute = false; // the link answers again: the ABORT that follows the cancel gets its reply
  await assert.rejects(p, (e) => e instanceof DkError && e.kind === 'cancelled');
  assert.equal((await s.info()).port, 'dn2');
  assert.equal(d.seen.includes('ABORT'), true);
});

test('another request waits for one window of the upload, not for the whole file', async () => {
  const d = new FakeDevice();
  const s = session(d, 2);
  const up = s.put('f', bytes(CH * 400));
  await new Promise((r) => setTimeout(r, 5));
  const before = d.dataSent;
  let after = -1;
  const poll = s.info().then(() => (after = d.dataSent));
  await Promise.all([up, poll]);
  assert.ok(before > 0 && before < 300, 'the put was under way');
  assert.ok(
    after - before <= 10,
    `INFO waited for ${after - before} DATA requests: one window of 8, plus the two in flight`,
  );
  assert.equal(d.ended, true);
});
