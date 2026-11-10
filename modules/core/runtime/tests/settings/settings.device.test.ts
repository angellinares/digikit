import { hasCap, type DkSession } from '@digi/dk-session';
import { describe, expect, test } from 'vitest';
import { statusOf } from '@digi/regression/runtime/card-kit.ts';
import { advance } from '@digi/regression/runtime/fx-live.ts';
import { DEVICES } from '@digi/regression/runtime/devices.ts';
import {
  decodeSettings,
  E_SET_KEY,
  E_SET_NOKEY,
  sectorOf,
  settingsMap,
  settingsMailbox,
  settingsRecord,
} from '@digi/regression/runtime/project-kit.ts';
import { ringEvents } from '@digi/regression/runtime/runtime-menu.ts';
import { AMOUNTS, byte, DELAY_MS, ROUTE, SECTOR, SEND, settingsCase, suiteName, text } from './settings-support.ts';

/**
 * The settings store of the runtime (docs/design/digikit-store.md "The settings record"; port of 6a5bb1ed:tools/patchkit/pk_digikit_settings.py).
 * A key put through the mailbox is written 2 s after the last change to the copy that is not current, and only there; a soft restart reads it back;
 * SETTINGS and CAPACITY report it; `patchkit store dump` decodes the card the guest left; a blank card is claimed first; a torn newer copy leaves the older one.
 * Route key persistence, error mapping with mocks and kit persistence are covered by fx-routes-persist, fault.test.ts and fx-kit-persist.
 * This file: the main flow and the restart. The blank card and the copy choice are in settings-copies, the older and overlapping stores in settings-cards.
 */

for (const device of DEVICES) {
  describe(suiteName(device), () => {
    const { s, settingsArea, kit, written, dump, dumpJson } = settingsCase(device);

    test('main: put and get, the 2 s delay, copy A then B, restart, remove', async () => {
      const rt = await kit.first();
      const { session } = rt;
      const usb = <T>(run: (dk: DkSession) => Promise<T>) => kit.usb(rt, run);
      const mb = settingsMailbox(session, kit.plan);
      const base = session.cardPath!;
      const left = (tag: string) => kit.after(tag, base);
      const events = () => ringEvents(session, kit.plan, 'SETTINGS_LOAD');

      let ev = await events();
      expect(ev.length, 'the boot read the settings area once').toBe(1);
      expect(ev[0]!.args, 'no valid copy (state 0, copy none, seq 0, 18 bytes, no entry)').toEqual([0, 2, 0, 18, 0]);
      let cap = (await usb((dk) => dk.capacity())).SETTINGS!;
      expect(cap.limit, 'CAPACITY reports SETTINGS in bytes (the area of one copy)').toBe(16 * SECTOR);
      expect(cap.used, 'CAPACITY used: the header').toBe(18);
      expect(hasCap(await usb((dk) => dk.info()), 'SETTINGS'), 'INFO has the capability').toBe(true);
      expect(await statusOf(() => usb(settingsRecord)), 'SETTINGS answers E_SET_NOKEY while there is no record').toBe(
        E_SET_NOKEY,
      );

      expect([await mb.put(ROUTE, byte(1)), await mb.put(SEND, AMOUNTS)], 'put').toEqual([0, 0]);
      expect(
        [await mb.get(ROUTE), await mb.get(SEND)],
        'put and get give the values back (route 1 byte, send amounts 16 bytes)',
      ).toEqual([
        [1, byte(1)],
        [16, AMOUNTS],
      ]);
      expect((await mb.get('fx/route/none'))[0], 'a key that is not there answers E_SET_NOKEY').toBe(E_SET_NOKEY);
      expect(
        [await mb.put('has space', text('x')), (await mb.get('has space'))[0], await mb.remove('has space')],
        'a key with a space answers E_SET_KEY (put, get, remove)',
      ).toEqual([E_SET_KEY, E_SET_KEY, E_SET_KEY]);
      expect(await mb.put(ROUTE, byte(1)), 'the same value again changes nothing').toBe(0);
      cap = (await usb((dk) => dk.capacity())).SETTINGS!;
      expect(cap.used, 'CAPACITY used is the header and the two entries (18 + 29 + 43)').toBe(
        18 + (1 + ROUTE.length + 2 + 1) + (1 + SEND.length + 2 + 16),
      );

      await advance(session, 1000);
      expect(await written(), 'nothing is written in the first second').toEqual([]);
      let rec = await usb(settingsRecord);
      expect(
        [rec.flags, rec.copy],
        "SETTINGS gives the RAM record before the write (flag 'not on the card', copy none)",
      ).toEqual([1, 2]);
      expect(settingsMap(decodeSettings(rec.raw)), 'the record has the two entries').toEqual({
        [ROUTE]: '01',
        [SEND]: AMOUNTS.toString('hex'),
      });

      // a second change at about 1 s: the 2 s start again
      await mb.put(ROUTE, byte(0));
      await advance(session, DELAY_MS - 500);
      expect(await written(), '1.5 s after the second change (2.5 s after the first) still nothing is written').toEqual(
        [],
      );
      await advance(session, 1000);
      expect(
        await written(),
        'about 2 s after the last change the record is written: settings copy A only (not B, not the selection, not the headers)',
      ).toEqual([s.setA]);
      rec = await usb(settingsRecord);
      expect([rec.flags, rec.copy, rec.seq], 'SETTINGS now says copy A, seq 1, nothing waiting').toEqual([0, 0, 1]);
      expect(settingsMap(decodeSettings(rec.raw))[ROUTE], 'the record has the last value').toBe('00');

      const after1 = await left('m1');
      const recA = sectorOf(after1, s.setA);
      expect(decodeSettings(recA)?.seq, 'copy A on the card is a valid record, seq 1').toBe(1);
      expect(recA.subarray(90).equals(Buffer.alloc(SECTOR - 90)), '90 bytes, zero after them').toBe(true);
      const shown = await dump(after1);
      const d1 = await dumpJson(after1);
      const sc = d1.settings;
      expect(sc.active, 'copy A in use').toBe('A');
      expect(sc.copies[0].seq).toBe(1);
      expect(sc.copies[1].state, 'copy B empty').toBe('empty');
      expect(
        Object.fromEntries(sc.copies[0].entries.map((e: { key: string; value: unknown }) => [e.key, e.value])),
        'the keys decoded (route master, the 16 amounts)',
      ).toEqual({ [ROUTE]: 'master', [SEND]: [...AMOUNTS] });
      expect(shown).toContain(`${SEND}  [${[...AMOUNTS].join(', ')}]`);
      expect(shown).toContain(`${ROUTE}  master (0)`);
      expect(shown).toContain('copy A: sector');

      // the next change goes to the other copy, A stays as it is
      const amounts2 = Buffer.alloc(16, 127);
      await mb.put(SEND, amounts2);
      await advance(session, DELAY_MS + 500);
      const after2 = await left('m2');
      expect(await written(), 'the next change goes to both settings copies written once').toEqual(settingsArea);
      expect(sectorOf(after2, s.setA).equals(recA), 'copy A is byte for byte what it was').toBe(true);
      const recB = decodeSettings(sectorOf(after2, s.setB));
      expect(recB?.seq, 'copy B has seq 2').toBe(2);
      expect(settingsMap(recB)[SEND]).toBe(amounts2.toString('hex'));

      const reboot = await kit.restart(rt);
      ev = await ringEvents(session, kit.plan, 'SETTINGS_LOAD');
      expect(ev.length).toBe(1);
      expect(ev[0]!.args.slice(0, 3), 'after a soft restart the boot reads copy B (state 1, copy 1, seq 2)').toEqual([
        1, 1, 2,
      ]);
      expect(ev[0]!.args[4], '2 entries').toBe(2);
      expect([await mb.get(ROUTE), await mb.get(SEND)], 'the values are the last ones').toEqual([
        [1, byte(0)],
        [16, amounts2],
      ]);
      rec = await kit.usb(reboot, settingsRecord);
      expect([rec.flags, rec.copy, rec.seq], 'SETTINGS after the restart: copy B, seq 2, nothing waiting').toEqual([
        0, 1, 2,
      ]);

      // the next change goes to A again (seq 3); a removed key stays removed
      await mb.put('fx/route/test/other', byte(1));
      await mb.remove(SEND);
      expect((await mb.get(SEND))[0], 'a removed key is gone at once').toBe(E_SET_NOKEY);
      await advance(session, DELAY_MS + 500);
      const after3 = await left('m3');
      const ra = decodeSettings(sectorOf(after3, s.setA));
      expect(ra?.seq, 'after the restart the next write goes to copy A (seq 3)').toBe(3);
      expect(settingsMap(ra), 'with the new key and without the removed one').toEqual({
        [ROUTE]: '00',
        'fx/route/test/other': '01',
      });
      expect(sectorOf(after3, s.setB).equals(sectorOf(after2, s.setB)), 'copy B is as it was').toBe(true);
      const all = await written();
      expect(
        all.filter((x) => x !== s.selA && x !== s.selB),
        'the only store sectors written are the two settings copies',
      ).toEqual(settingsArea);
    });

    test('restart: a put and at once a restart keeps the value (dkrt_restart writes the pending record first)', async () => {
      const rt = await kit.first();
      const { session } = rt;
      const mb = settingsMailbox(session, kit.plan);
      await mb.put(ROUTE, byte(1));
      const rec = await kit.usb(rt, settingsRecord);
      expect(rec.flags & 1, 'right after a put the change is not on the card yet (SETTINGS flag bit 0)').toBe(1);
      expect((await written()).filter((x) => settingsArea.includes(x))).toEqual([]);
      await kit.restart(rt);
      const ev = await ringEvents(session, kit.plan, 'SETTINGS_LOAD');
      expect(ev.length).toBe(1);
      expect(ev[0]!.args.slice(0, 3), 'the boot reads copy A (state 1, copy 0, seq 1)').toEqual([1, 0, 1]);
      expect(ev[0]!.args[4], '1 entry').toBe(1);
      expect(await mb.get(ROUTE), 'the value is there').toEqual([1, byte(1)]);
      const only = (await written()).filter((x) => x !== s.selA && x !== s.selB);
      expect(only, 'the only store sector written besides the selection record is settings copy A').toEqual([s.setA]);
    });
  });
}
