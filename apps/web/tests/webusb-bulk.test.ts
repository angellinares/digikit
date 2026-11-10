// The WebUSB bulk link (src/modules/webusb-bulk.ts) over a fake navigator.usb whose device is the FakeBulkDevice at the level of USB transfers: devices come
// from getDevices() (the chooser is only for requestAccess), a frame that is a multiple of 512 bytes gets its zero-length OUT transfer, replies come in
// packets with a zero-length IN transfer after a 512-multiple one, a stalled endpoint, an unplugged device, and the nonce handshake over two devices.
import { afterEach, expect, test } from 'vitest';
import { BulkSession, bulkReply, bulkRequest, chooseBulk, fileHash } from '@digi/dk-session';
import { FakeBulkDevice } from '@digi/dk-session/bulk-fake';
import {
  NO_WEBUSB,
  WebUsbBulkLink,
  findBulk,
  webUsbAvailable,
  webUsbOpener,
  type UsbDeviceLike,
  type UsbLike,
} from '../src/modules/webusb-bulk.ts';

const bytes = (n: number) => Uint8Array.from({ length: n }, (_, i) => (i * 131 + 7) & 255);
const hash = (d: Uint8Array) => new DataView(fileHash(d).buffer, 0, 4).getUint32(0);

/** A USB device that is the fake bulk device. IN data comes in 512-byte packets (a zero-length one after a 512-multiple frame, as the device's ZLT does). */
function usbDevice(dev: FakeBulkDevice, o: { stallOut?: number } = {}) {
  const log = { claimed: [] as number[], released: [] as number[], closed: 0, outs: [] as number[], stalls: 0 };
  const pieces: Uint8Array[] = [];
  let wake: (() => void) | null = null;
  let unplugged = false;
  let stallOut = o.stallOut ?? 0;
  const device: UsbDeviceLike = {
    vendorId: 0x1935,
    productName: 'Digitone II',
    opened: false,
    configuration: {
      interfaces: [
        { interfaceNumber: 0, alternates: [{ alternateSetting: 0, interfaceClass: 1, endpoints: [] }] },
        {
          interfaceNumber: 2,
          alternates: [
            {
              alternateSetting: 0,
              interfaceClass: 0xff,
              endpoints: [
                { endpointNumber: 2, direction: 'out', type: 'bulk' },
                { endpointNumber: 2, direction: 'in', type: 'bulk' },
              ],
            },
          ],
        },
      ],
    },
    async open() {
      device.opened = true;
    },
    async close() {
      log.closed++;
      wake?.();
    },
    async selectConfiguration() {},
    async claimInterface(n) {
      log.claimed.push(n);
    },
    async releaseInterface(n) {
      log.released.push(n);
    },
    async clearHalt() {
      log.stalls++;
    },
    async transferOut(_ep, data) {
      if (unplugged) throw new Error('NotFoundError: the device was disconnected');
      if (stallOut > 0) {
        stallOut--;
        return { status: 'stall' };
      }
      log.outs.push(data.length);
      dev.out(data.slice());
      for (let r; (r = await dev.recv(0));) pieces.push(...dev.inTransfers(r));
      wake?.();
      return { status: 'ok', bytesWritten: data.length };
    },
    async transferIn() {
      while (!pieces.length) {
        if (unplugged || log.closed) throw new Error('NotFoundError: the device was disconnected');
        await new Promise<void>((r) => (wake = r));
      }
      const p = pieces.shift()!;
      return { status: 'ok', data: new DataView(p.buffer, p.byteOffset, p.byteLength) };
    },
  };
  return { device, log, unplug: () => ((unplugged = true), wake?.()) };
}

const usbWith = (...ds: UsbDeviceLike[]) => {
  const asked: unknown[] = [];
  const usb: UsbLike = { getDevices: async () => ds, requestDevice: async (o) => (asked.push(o), ds[0]!) };
  return { usb, asked };
};

afterEach(() => void delete (globalThis as { navigator?: unknown }).navigator);

test('open uses the devices the page may use and never opens the chooser; requestAccess does, with the vendor filters', async () => {
  const f = usbDevice(new FakeBulkDevice());
  const { usb, asked } = usbWith(f.device);
  const opener = webUsbOpener(usb);
  const c = await opener.candidates();
  expect(c).toEqual([{ id: '0', name: 'Digitone II' }]);
  const link = await opener.open('0');
  expect(asked).toEqual([]);
  expect(f.log.claimed).toEqual([2]);
  expect(findBulk(f.device)).toMatchObject({ interfaceNumber: 2, out: 2, inp: 2 });
  await link.close();
  expect(f.log.released).toEqual([2]);
  await WebUsbBulkLink.requestAccess(usb);
  expect(asked).toEqual([{ filters: [{ vendorId: 0x1935, classCode: 0xff }] }]);
});

test('a put runs through WebUSB over transfers: a 512-multiple frame gets a zero-length OUT transfer', async () => {
  const dev = new FakeBulkDevice();
  const f = usbDevice(dev);
  const link = await WebUsbBulkLink.open(f.device);
  const s = new BulkSession(link, { chunk: 4096, timeoutMs: 500 });
  const data = bytes(9 * 4096 + 5);
  expect(await s.put(1, 'drums/kick', data, { expected: hash(data) })).toBe(hash(data));
  expect(dev.files.get('drums/kick')).toEqual(data);
  // BEGIN of exactly 512 bytes: 12 header + 4 root + path + NUL + 4 size + 4 hash
  const path = 'p'.repeat(512 - 12 - 4 - 1 - 8);
  f.log.outs.length = 0;
  await s.put(1, path, new Uint8Array());
  expect(f.log.outs.slice(0, 2)).toEqual([512, 0]);
  expect(dev.files.has(path)).toBe(true);
  expect(dev.badFrames).toBe(0);
  await link.close();
});

test('a reply that is a multiple of 512 bytes is followed by a zero-length IN transfer that is no bytes', async () => {
  const dev = new FakeBulkDevice();
  dev.infoPad = 512 - 12 - 20; // an INFO reply of exactly 512 bytes
  dev.nonce = Uint8Array.of(1, 2, 3, 4, 5, 6, 7, 8);
  const f = usbDevice(dev);
  const link = await WebUsbBulkLink.open(f.device);
  const s = new BulkSession(link, { timeoutMs: 500 });
  expect([...(await s.info()).nonce]).toEqual([1, 2, 3, 4, 5, 6, 7, 8]);
  expect((await s.info()).align).toBe(512); // the ZLT after the first reply did not end or break the stream
  await link.close();
});

test('a stalled OUT endpoint is cleared and reported; the link works for the next request', async () => {
  const f = usbDevice(new FakeBulkDevice(), { stallOut: 1 });
  const link = await WebUsbBulkLink.open(f.device);
  await expect(link.send(bulkRequest(1, 0))).rejects.toThrow(/stall/);
  expect(f.log.stalls).toBe(1);
  const s = new BulkSession(link, { timeoutMs: 500 });
  expect((await s.info()).align).toBe(512);
  await link.close();
});

test('recv ends on a timeout and on an abort', async () => {
  const f = usbDevice(new FakeBulkDevice());
  const link = await WebUsbBulkLink.open(f.device);
  expect(await link.recv(20)).toBeNull();
  const ctl = new AbortController();
  const p = link.recv(10_000, ctl.signal);
  ctl.abort();
  expect(await p).toBeNull();
  await link.close();
});

test('an unplugged device fails send and recv with its reason, and a closed link says so', async () => {
  const f = usbDevice(new FakeBulkDevice());
  const link = await WebUsbBulkLink.open(f.device);
  f.unplug();
  await expect(link.recv(500)).rejects.toThrow(/disconnected/);
  await expect(link.send(new Uint8Array(12))).rejects.toThrow(/disconnected/);
});

test('a corrupt IN stream fails the link', async () => {
  const f = usbDevice(new FakeBulkDevice());
  const orig = f.device.transferIn;
  let first = true;
  f.device.transferIn = async (ep, n) => {
    if (first) {
      first = false;
      const junk = new Uint8Array(12);
      return { status: 'ok', data: new DataView(junk.buffer) };
    }
    return orig(ep, n);
  };
  const link = await WebUsbBulkLink.open(f.device);
  await expect(link.recv(500)).rejects.toThrow(/magic/);
});

test('a device without the bulk interface is refused and closed', async () => {
  const f = usbDevice(new FakeBulkDevice());
  f.device.configuration!.interfaces.pop();
  await expect(WebUsbBulkLink.open(f.device)).rejects.toThrow(/no bulk interface/);
  expect(f.log.closed).toBe(1);
  expect(await webUsbOpener(usbWith(f.device).usb).candidates()).toEqual([]);
});

test('two devices: the nonce handshake picks the one on the MIDI port', async () => {
  const devs = [new FakeBulkDevice(), new FakeBulkDevice()];
  const usbs = devs.map((d) => usbDevice(d));
  const opener = webUsbOpener(usbWith(...usbs.map((u) => u.device)).usb);
  for (const target of [0, 1]) {
    const c = await chooseBulk({ usbFastNonce: async (n) => devs[target]!.setNonce(n) }, opener, undefined, {
      timeoutMs: 500,
    });
    expect(c?.id).toBe(String(target));
    await c?.link.close();
  }
  // the MIDI target is another device: none matches, SysEx
  const notes: string[] = [];
  expect(
    await chooseBulk({ usbFastNonce: async () => {} }, opener, (n) => notes.push(n), { timeoutMs: 500 }),
  ).toBeUndefined();
  expect(notes.at(-1)).toMatch(/none of 2/);
});

test('no WebUSB: a readable error, and the env offers no bulk link', async () => {
  expect(webUsbAvailable()).toBe(false);
  await expect(WebUsbBulkLink.requestAccess(undefined)).rejects.toThrow(NO_WEBUSB);
  expect(await webUsbOpener(undefined).candidates()).toEqual([]);
  (globalThis as { navigator?: unknown }).navigator = { usb: usbWith(usbDevice(new FakeBulkDevice()).device).usb };
  expect(webUsbAvailable()).toBe(true);
});

test('bulkReply and bulkRequest are exported for device-side tests', () => {
  expect(bulkReply(1, 2, 0).length).toBe(12);
});
