// The emulator's bulk link: a candidate only while the guest's USB MIDI port entry is taken over by the adapter, the bus reset once per boot of the guest, the
// zero-length packet as its own transfer, and whole frames out of the capture.
import { describe, expect, test } from 'vitest';
import { EmuBulkLink, bulkReply, bulkRequest } from '../src/index.ts';
import { BULK_CMD } from '../src/gen_bulk.ts';

const TABLE = 0x4029ef80;
const hexOf = (b: Uint8Array) => Array.from(b, (x) => x.toString(16).padStart(2, '0')).join('');

/** A control daemon that knows the port table, the capture and a boot counter. */
function fakeDaemon(connect: number, epoch = { n: 1 }) {
  const calls: { method: string; params: Record<string, unknown> }[] = [];
  const out: string[] = [];
  const call = async <T>(method: string, params: Record<string, unknown> = {}): Promise<T> => {
    calls.push({ method, params });
    if (method === 'mem.cf.peek') {
      const addr = params.addr as number;
      return { value: addr === 0x40006c38 ? TABLE : addr === TABLE + 2 * 0x28 ? connect : null } as T;
    }
    if (method === 'status') return { epoch: epoch.n } as T;
    if (method === 'usb.bulk.out') return { hex: out.shift() ?? '' } as T;
    return {} as T;
  };
  return { call, calls, out, epoch };
}

describe('EmuBulkLink', () => {
  test('there is a candidate only when the adapter has taken port entry 2 over', async () => {
    expect(await EmuBulkLink.opener(fakeDaemon(0x400044b8).call).candidates(), 'the stock connect callback').toEqual(
      [],
    );
    expect(await EmuBulkLink.opener(fakeDaemon(0x4000a000).call).candidates()).toEqual([
      { id: 'emulator', name: 'emulator' },
    ]);
  });

  test('the bus reset goes with the first frame of a boot only; a frame of a multiple of 512 bytes is followed by its zero-length packet', async () => {
    const d = fakeDaemon(0x4000a000);
    const first = new EmuBulkLink(d.call);
    await first.send(bulkRequest(BULK_CMD.INFO, 1));
    await first.send(bulkRequest(BULK_CMD.DATA, 2, new Uint8Array(512 - 12)));
    const ins = d.calls.filter((c) => c.method === 'usb.bulk.in');
    expect(ins.map((c) => c.params.connect)).toEqual([0x4000a000, undefined, undefined]);
    expect(ins.map((c) => (c.params.bytes ? String(c.params.bytes).length / 2 : 0))).toEqual([12, 512, 0]);
    expect(ins[2]!.params.zlp, 'the empty transfer is the packet').toBe(true);
    // another link of the same boot does not reset the bus again; a new boot of the guest does
    await new EmuBulkLink(d.call).send(bulkRequest(BULK_CMD.INFO, 3));
    expect(d.calls.filter((c) => c.method === 'usb.bulk.in').pop()!.params.connect).toBeUndefined();
    d.epoch.n = 2;
    await new EmuBulkLink(d.call).send(bulkRequest(BULK_CMD.INFO, 4));
    expect(d.calls.filter((c) => c.method === 'usb.bulk.in').pop()!.params.connect).toBe(0x4000a000);
  });

  test('replies arrive as whole frames, even when the capture cuts them', async () => {
    const d = fakeDaemon(0x4000a000);
    const a = bulkReply(BULK_CMD.INFO, 1, 0, new Uint8Array(20));
    const b = bulkReply(BULK_CMD.END, 2, 0, new Uint8Array(4));
    const all = new Uint8Array([...a, ...b]);
    d.out.push(hexOf(all.subarray(0, 7)), hexOf(all.subarray(7, 40)), hexOf(all.subarray(40)));
    const link = new EmuBulkLink(d.call);
    expect(await link.recv(1000)).toEqual(a);
    expect(await link.recv(1000)).toEqual(b);
    expect(await link.recv(1), 'nothing more').toBeNull();
  });
});
