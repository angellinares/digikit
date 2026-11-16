// The first stock MidiRpc request after restart(): the firmware drops stock requests during its intro, so the session asks the read-only Screenshot first
// and a write is never the request that is lost.
import assert from 'node:assert/strict';
import { test } from 'vitest';
import { DkSession, encode8in7, parseRpcReply, rpcRequest } from '@digi/dk-session';
import { CMD as GCMD } from '@digi/dk-session/gen_proto';

const CREATE_DIR = 0x11;
const SCREENSHOT = 0x04;

/** A device that answers digikit commands, and stock requests only once `introEnds` stock requests went by (dropped while the intro runs). */
class Device {
  stockSeen: number[] = [];
  dropped = 0;
  readonly introEnds: number;
  constructor(introEnds: number) {
    this.introEnds = introEnds;
  }
  private queue: Uint8Array[] = [];
  async send(msg: Uint8Array) {
    if (msg[4] === 0x10) {
      const type = parseRpcReply(msg)![0];
      this.stockSeen.push(type);
      if (this.dropped < this.introEnds) return void this.dropped++;
      return void this.queue.push(rpcRequest(type, 0, Uint8Array.of(0)));
    }
    const reply = Uint8Array.from([
      0xf0,
      0x7d,
      0x64,
      0x6b,
      1,
      msg[5]! | 0x40,
      msg[6]!,
      ...encode8in7(Uint8Array.of(0)),
      0xf7,
    ]);
    if (msg[5] === GCMD.RESTART) this.queue.push(reply);
  }
  async recv() {
    return this.queue.shift() ?? null;
  }
}

test('the first stock request after restart waits for the stock service, no write is lost', async () => {
  const dev = new Device(3);
  const dk = new DkSession(dev, { timeoutMs: 40, retries: 0 });
  await dk.restart();
  const got = await dk.rpc(CREATE_DIR, Uint8Array.of(0), 40);
  assert.deepEqual([...got], [0]);
  assert.deepEqual(dev.stockSeen, [SCREENSHOT, SCREENSHOT, SCREENSHOT, SCREENSHOT, CREATE_DIR]);
  // proven: the next request goes out alone
  await dk.rpc(CREATE_DIR, Uint8Array.of(0), 40);
  assert.equal(dev.stockSeen.length, 6);
});

test('without a restart a stock request goes out alone', async () => {
  const dev = new Device(0);
  const dk = new DkSession(dev, { timeoutMs: 40, retries: 0 });
  await dk.rpc(CREATE_DIR, Uint8Array.of(0), 40);
  assert.deepEqual(dev.stockSeen, [CREATE_DIR]);
});
