import zlib from 'node:zlib';
import { cardRig, RT } from '@digi/regression/runtime/card-rig.ts';
import { overlayArgs } from '@digi/regression/runtime/card-kit.ts';
import { cardStore, deviceProfile } from '@digi/regression/runtime/device-data.ts';
import { FIRMWARE } from '@digi/regression/runtime/devices.ts';
import type { Device } from '@digi/test-support';
import { patchkit } from '@digi/regression/runtime/patchkit.ts';
import { RING_SYMBOLS } from '@digi/regression/runtime/runtime-menu.ts';

/** Shared by settings.device.test.ts, settings-copies.device.test.ts and settings-cards.device.test.ts (one file per group of independent tests). */

export const ROUTE = 'fx/route/test/dyn-fx-gain';
export const SEND = 'fx/send/test/dyn-fx-gain';
export const AMOUNTS = Buffer.from([0, 127, 64, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 100]);
export const E_BAD_STORE = -2;
export const DELAY_MS = 2000; // the runtime writes the record this long after the last change
export const SECTOR = 512;
export const MODULE = 'test/c-demo';
export const MODULE_PATH = 'modules/c-demo.dkm';
export const byte = (n: number) => Buffer.from([n]);
export const text = (s: string) => Buffer.from(s);
export const padded = (b: Buffer) => Buffer.concat([b, Buffer.alloc(SECTOR - b.length)]);

/** A pattern that is not a record, not a header and not blank. */
export const PATTERN = Buffer.from(Array.from({ length: SECTOR }, (_, i) => (i * 7 + 3) & 0xff));

/** A store header of an older store (written before the settings area), checksummed. */
export function storeHeader(count: number, tableCrc: number, seq: number, used: number, sectors: number): Buffer {
  const b = Buffer.alloc(SECTOR);
  b.write('DKST', 0);
  [4, count, 1, 1, tableCrc, seq, used, sectors].forEach((v, i) => b.writeUInt32BE(v, 4 + 4 * i));
  b.writeUInt32BE(zlib.crc32(b.subarray(0, 508)), 508);
  return b;
}

/** A copy with one bit pattern flipped so that its CRC fails. */
export const torn = (b: Buffer) => {
  const x = Buffer.from(b);
  x[20] = x[20]! ^ 0x55;
  return x;
};

export const suiteName = (device: Device) => `${device.toUpperCase()} ${FIRMWARE[device]} settings store`;

/** The card rig of a settings describe block, with the store layout of the device and the patchkit dump helpers. */
export function settingsCase(device: Device) {
  const s = cardStore(device);
  const first = s.first;
  const headerB = deviceProfile(device).card.store.header_b_sector;
  const limit = headerB - first;
  const sectors = s.end - first;
  const settingsArea = [s.setA, s.setB];
  const kit = cardRig(device, 'settings', {
    dsp: 'zero',
    roots: RT,
    symbols: ['DKRT_MAILBOX', 'DKRT_CARD', ...RING_SYMBOLS],
    // an empty selection record: the area is ours, the settings copies are zero
    initial: () => ({ selection: [] }),
    // the settings copies and the empty header pair of a claimed blank area
    storeExtra: [...settingsArea, first, first + limit],
  });
  const written = () => kit.storeWrites();
  const dump = async (card: string, ...flags: string[]) =>
    (await patchkit(['store', 'dump', card, ...overlayArgs(card), '--device', device, ...flags])).stdout;
  const dumpJson = async (card: string) => JSON.parse(await dump(card, '--json'));
  return { s, first, headerB, limit, sectors, settingsArea, kit, written, dump, dumpJson };
}
