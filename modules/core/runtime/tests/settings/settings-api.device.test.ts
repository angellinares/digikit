import type { DkSession } from '@digi/dk-session';
import { describe, expect, test } from 'vitest';
import { bootInfo, statusOf } from '@digi/regression/runtime/card-kit.ts';
import type { PatchedSession } from '@digi/regression/support.ts';
import { cardRig, RT } from '@digi/regression/runtime/card-rig.ts';
import { cardStore } from '@digi/regression/runtime/device-data.ts';
import { advance } from '@digi/regression/runtime/fx-live.ts';
import { DEVICES, FIRMWARE } from '@digi/regression/runtime/devices.ts';
import {
  decodeSettings,
  E_SET_KEY,
  E_SET_NOKEY,
  settingsMap,
  settingsRecord,
} from '@digi/regression/runtime/project-kit.ts';
import { RING_SYMBOLS } from '@digi/regression/runtime/runtime-menu.ts';

/**
 * The module settings API (api 1.9, <dk/settings.h>). The fixtures test/settings-a and test/settings-b are one source: a service command queues a put, get
 * or remove request, slot 0 runs it in the UI task, and `dkm_init` reads the key "boot". Keys are stored as `<module name>/<key>`.
 */

const A = 'test/settings-a';
const B = 'test/settings-b';
const SELECTION = ['modules/settings-a.dkm', 'modules/settings-b.dkm'];
const CMD = { [A]: 0x38, [B]: 0x3a };
const PUT = 1;
const GET = 2;
const REMOVE = 3;
const DELAY_MS = 2000; // the runtime writes the record this long after the last change
const text = (s: string) => Buffer.from(s);
const hex = (s: string) => text(s).toString('hex');

for (const device of DEVICES) {
  describe(`${device.toUpperCase()} ${FIRMWARE[device]} module settings API`, () => {
    const s = cardStore(device);
    const kit = cardRig(device, 'settings-api', {
      dsp: 'zero',
      roots: RT,
      symbols: ['DKRT_MAILBOX', 'DKRT_CARD', ...RING_SYMBOLS],
      initial: () => ({ selection: [] }),
      storeExtra: [s.setA, s.setB],
    });
    const spec = { modules: ['test/settings-a', 'test/settings-b'], selection: SELECTION };

    /** One request of a fixture module: queued by the service command, run by slot 0, then read back. */
    async function call(
      rt: PatchedSession,
      module: typeof A | typeof B,
      op: number,
      key: string,
      value = Buffer.alloc(0),
    ) {
      const usb = <T>(run: (dk: DkSession) => Promise<T>) => kit.usb(rt, run);
      const body = Buffer.concat([Buffer.from([op, key.length]), text(key), value]);
      expect((await usb((dk) => dk.request(CMD[module], body))).status, 'the request is queued').toBe(0);
      await advance(rt.session, 200); // slot 0 runs about 120 times a second
      const r = await usb((dk) => dk.request(CMD[module] + 1));
      const b = Buffer.from(r.body);
      expect(b[0], 'slot 0 ran the request').toBe(0);
      const rc = b.readInt32BE(1);
      return { rc, initRc: b.readInt32BE(5), init: b.subarray(9, 25), val: b.subarray(25, 25 + Math.max(rc, 0)) };
    }
    const record = async (rt: PatchedSession) =>
      settingsMap(decodeSettings((await kit.usb(rt, settingsRecord)).raw));

    test('a value put through the API is on the card after a restart and is read in dkm_init', async () => {
      const rt = await kit.load('persist', spec);
      expect((await bootInfo(rt.session, kit.plan)).results, 'both modules load').toEqual([0, 0]);
      const first = await call(rt, A, GET, 'boot');
      expect([first.initRc, first.rc], 'nothing is saved yet: dkm_init and get answer E_SET_NOKEY').toEqual([
        E_SET_NOKEY,
        E_SET_NOKEY,
      ]);
      expect((await call(rt, A, PUT, 'boot', text('xyz'))).rc, 'put').toBe(0);
      expect((await call(rt, A, GET, 'boot')).val, 'get gives the value back').toEqual(text('xyz'));
      expect(await record(rt), 'the stored key has the module name in front').toEqual({ [`${A}/boot`]: hex('xyz') });
      await advance(rt.session, DELAY_MS + 500);
      const reboot = await kit.restart(rt);
      const again = await call(reboot, A, GET, 'boot');
      expect(again.initRc, 'dkm_init read 3 bytes').toBe(3);
      expect(again.init.subarray(0, 3), 'the value of the earlier boot').toEqual(text('xyz'));
      expect(await record(reboot), 'the record is as it was').toEqual({ [`${A}/boot`]: hex('xyz') });
      kit.allowStoreWrites(await kit.storeWrites());
    }, 900_000);

    test('the same key in two modules gives two records', async () => {
      const rt = await kit.load('collide', spec);
      expect((await call(rt, A, PUT, 'k', text('A1'))).rc).toBe(0);
      expect((await call(rt, B, PUT, 'k', text('B2'))).rc).toBe(0);
      expect(await record(rt), 'one stored key per module name').toEqual({
        [`${A}/k`]: hex('A1'),
        [`${B}/k`]: hex('B2'),
      });
      expect([(await call(rt, A, GET, 'k')).val, (await call(rt, B, GET, 'k')).val]).toEqual([text('A1'), text('B2')]);
      expect((await call(rt, B, REMOVE, 'k')).rc, 'remove').toBe(0);
      expect(await record(rt), 'removing B leaves the key of A').toEqual({ [`${A}/k`]: hex('A1') });
      expect((await call(rt, B, GET, 'k')).rc).toBe(E_SET_NOKEY);
      await advance(rt.session, DELAY_MS + 500);
      kit.allowStoreWrites(await kit.storeWrites());
    }, 900_000);

    test('a key that is too long, leaves its prefix or breaks the rules is refused and nothing is written', async () => {
      const rt = await kit.load('refused', spec);
      const bad = ['k'.repeat(40), `../${B}/k`, 'a/../b', '/other', 'has space', ''];
      for (const key of bad) {
        expect((await call(rt, A, PUT, key, text('x'))).rc, `put ${JSON.stringify(key)}`).toBe(E_SET_KEY);
        expect((await call(rt, A, GET, key)).rc, `get ${JSON.stringify(key)}`).toBe(E_SET_KEY);
        expect((await call(rt, A, REMOVE, key)).rc, `remove ${JSON.stringify(key)}`).toBe(E_SET_KEY);
      }
      await advance(rt.session, DELAY_MS + 500);
      expect(
        (await kit.storeWrites()).filter((x) => x !== s.selA && x !== s.selB),
        'no store sector is written besides the selection record (boot counter)',
      ).toEqual([]);
      expect(
        await statusOf(() => kit.usb(rt, settingsRecord)),
        'the settings record is still empty (SETTINGS answers E_SET_NOKEY)',
      ).toBe(E_SET_NOKEY);
    }, 900_000);
  });
}
