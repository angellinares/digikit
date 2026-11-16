// INFO: the ABI fingerprint of service files follows the image id, and is read only when the runtime sets the capability SVC_ABI.
import assert from 'node:assert/strict';
import { test } from 'vitest';
import { CAP } from '../src/gen_proto.ts';
import { DkSession, decode8in7, encode8in7 } from '../src/index.ts';

const concat = (...p: Uint8Array[]) => Uint8Array.from(Buffer.concat(p.map((x) => Buffer.from(x))));

/** The INFO body of a runtime: `port` "dn2", the log range zero, `caps`, an image id and, when `abi` is given, the fingerprint after the id. */
function info(caps: number, abi?: number): Uint8Array {
  const b = new Uint8Array(abi === undefined ? 41 : 45);
  const v = new DataView(b.buffer);
  v.setUint16(0, 1);
  v.setUint16(2, 4);
  b.set(new TextEncoder().encode('dn2'), 4);
  v.setUint32(25, 0x000500);
  v.setUint32(29, caps);
  b.set([1, 2, 3, 4, 5, 6, 7, 8], 33);
  if (abi !== undefined) v.setUint32(41, abi);
  return b;
}

class InfoDevice {
  dataDepth: number | undefined;
  private queue: Uint8Array[] = [];
  private body: Uint8Array;
  constructor(body: Uint8Array) {
    this.body = body;
  }
  send(msg: Uint8Array) {
    const cmd = msg[5]!;
    const seq = msg[6]!;
    decode8in7(msg.subarray(7, msg.length - 1));
    this.queue.push(
      concat(
        Uint8Array.of(0xf0, 0x7d, 0x64, 0x6b, 1, cmd | 0x40, seq),
        encode8in7(concat(Uint8Array.of(0), this.body)),
        Uint8Array.of(0xf7),
      ),
    );
  }
  async recv(): Promise<Uint8Array | null> {
    return this.queue.shift() ?? null;
  }
}

const read = (body: Uint8Array) => new DkSession(new InfoDevice(body), { timeoutMs: 200, retries: 0 }).info();

test('the fingerprint after the image id is read when the capability is set', async () => {
  const i = await read(info(1 << CAP.SVC_ABI, 0xdeadbeef));
  assert.equal(i.svc_abi, 0xdeadbeef);
  assert.equal(i.image_id, '0102030405060708');
});

test('a runtime without the capability, or with a shorter reply, reports none', async () => {
  assert.equal((await read(info(0, 0xdeadbeef))).svc_abi, null);
  assert.equal((await read(info(1 << CAP.SVC_ABI))).svc_abi, null);
});
