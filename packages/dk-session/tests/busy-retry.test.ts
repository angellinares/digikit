// E_BUSY: the device's store is in use (an SDK file write on its I/O task). The request did nothing: `DkSession` asks again after a short wait.
import assert from 'node:assert/strict';
import { test } from 'vitest';
import { BUSY_RETRIES, DkError, DkSession, decode8in7, encode8in7 } from '../src/index.ts';

const E_BUSY = -12;
const E_OFFSET = -54;
const CMD = { LIST: 0x02, DATA: 0x11, SERVICE: 0x38 };
const concat = (...p: Uint8Array[]) => Uint8Array.from(Buffer.concat(p.map((x) => Buffer.from(x))));
const reply = (cmd: number, seq: number, status: number) =>
  concat(
    Uint8Array.of(0xf0, 0x7d, 0x64, 0x6b, 1, cmd | 0x40, seq),
    encode8in7(Uint8Array.of(status & 255)),
    Uint8Array.of(0xf7),
  );
const u32 = (b: Uint8Array, at = 0) => new DataView(b.buffer, b.byteOffset + at, 4).getUint32(0);

/** A device that answers E_BUSY to the first `busy` requests of the store commands, then takes them (DATA at the next offset, like the runtime). */
class BusyDevice {
  dataDepth: number | undefined;
  requests = 0;
  busyLeft: number;
  next = 0;
  private queue: Uint8Array[] = [];
  private waiter: (() => void) | null = null;
  constructor(busy: number, depth?: number) {
    this.busyLeft = busy;
    this.dataDepth = depth;
  }
  send(msg: Uint8Array) {
    const cmd = msg[5]!;
    const seq = msg[6]!;
    const body = decode8in7(msg.subarray(7, msg.length - 1));
    this.requests++;
    let status = 0;
    if (this.busyLeft > 0) {
      this.busyLeft--;
      status = E_BUSY;
    } else if (cmd === CMD.DATA) {
      const off = u32(body);
      if (off !== this.next) status = E_OFFSET;
      else this.next = off + body.length - 4;
    }
    this.queue.push(reply(cmd, seq, status));
    this.waiter?.();
  }
  async recv(timeoutMs: number): Promise<Uint8Array | null> {
    if (this.queue.length === 0) {
      await new Promise<void>((resolve) => {
        const done = () => {
          clearTimeout(timer);
          this.waiter = null;
          resolve();
        };
        const timer = setTimeout(done, timeoutMs);
        this.waiter = done;
      });
    }
    return this.queue.shift() ?? null;
  }
}

test('a request that gets E_BUSY is sent again until the store is free', async () => {
  const d = new BusyDevice(3);
  const s = new DkSession(d, { timeoutMs: 200, retries: 0 });
  const r = await s.request(CMD.LIST, new Uint8Array(4));
  assert.equal(r.status, 0);
  assert.equal(d.requests, 4);
});

test('E_BUSY for a DATA chunk with another one in flight: the chunks are sent again in order', async () => {
  const d = new BusyDevice(1, 2);
  const s = new DkSession(d, { timeoutMs: 200, retries: 0, chunk: 512, depth: 2 });
  const data = new Uint8Array(512 * 4 + 100).map((_, i) => i & 255);
  // BEGIN is not modelled: the fake takes DATA at the next offset whatever came before
  const chunks = [0, 1, 2, 3, 4].map((k) =>
    concat(Uint8Array.of(0, 0, (k * 512) >> 8, 0), data.subarray(k * 512, Math.min(data.length, k * 512 + 512))),
  );
  const sent: number[] = [];
  const window = s as unknown as {
    withTurn: (f: (sig: AbortSignal) => Promise<void>) => Promise<void>;
    dataWindow: Function;
  };
  await window.withTurn((signal) =>
    window.dataWindow.call(
      s,
      chunks.map((body, k) => ({ off: k * 512, body })),
      (i: number) => sent.push(i),
      signal,
    ),
  );
  assert.deepEqual(sent, [0, 1, 2, 3, 4]);
  assert.equal(d.next, data.length);
});

test('E_BUSY past the retry bound reaches the caller', async () => {
  const d = new BusyDevice(1000);
  const s = new DkSession(d, { timeoutMs: 200, retries: 0 });
  const r = await s.request(CMD.LIST, new Uint8Array(4));
  assert.equal(r.status, E_BUSY);
  assert.equal(d.requests, BUSY_RETRIES + 1);
  await assert.rejects(s.delete('x'), (e: unknown) => e instanceof DkError && e.status === 'E_BUSY');
}, 15000);

test("E_BUSY of a module command is the module's own answer: it is not asked again", async () => {
  const d = new BusyDevice(1000);
  const s = new DkSession(d, { timeoutMs: 200, retries: 0 });
  const r = await s.request(CMD.SERVICE, new Uint8Array(4));
  assert.equal(r.status, E_BUSY);
  assert.equal(d.requests, 1);
});

test('the quick INFO probe does not wait on E_BUSY', async () => {
  const d = new BusyDevice(1000);
  const s = new DkSession(d, { timeoutMs: 200, retries: 2 });
  const r = await s.request(0x01, new Uint8Array(), true);
  assert.equal(r.status, E_BUSY);
  assert.equal(d.requests, 1);
});
