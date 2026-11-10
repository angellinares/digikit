import fs from 'node:fs';
import path from 'node:path';
import zlib from 'node:zlib';
import type { DkSession } from '@digi/dk-session';
import { describe, expect, test } from 'vitest';
import { bootInfo, buildCard, E_NOSTORE, statusOf } from '@digi/regression/runtime/card-kit.ts';
import { advance } from '@digi/regression/runtime/fx-live.ts';
import { DEVICES } from '@digi/regression/runtime/devices.ts';
import { moduleArtifact } from '@digi/regression/runtime/install.ts';
import { E_SET_OFF, sectorOf, settingsMailbox, settingsRecord } from '@digi/regression/runtime/project-kit.ts';
import { ringEvents } from '@digi/regression/runtime/runtime-menu.ts';
import {
  byte,
  DELAY_MS,
  E_BAD_STORE,
  MODULE,
  MODULE_PATH,
  padded,
  PATTERN,
  ROUTE,
  SECTOR,
  settingsCase,
  storeHeader,
  suiteName,
} from './settings-support.ts';

/** The settings store, part 3 (see settings.device.test.ts): a store that overlaps the settings area, and a card with the older header B. */

for (const device of DEVICES) {
  describe(suiteName(device), () => {
    const { s, first, headerB, limit, sectors, kit, written, dumpJson } = settingsCase(device);

    test('overlap: an older store with a file in the settings area turns the settings store off', async () => {
      // an older store (allocator ended at sectors - 3) with a file "x" from 2 sectors before header B for 4 sectors: it covers header B and the first sector of settings copy A
      const entry = Buffer.alloc(256);
      entry.write('x', 0);
      const firstRel = limit - 2;
      entry.writeUInt32BE(firstRel, 160);
      entry.writeUInt32BE(4 * SECTOR, 164);
      const table = padded(entry);
      const foreign: Record<number, Buffer> = {
        [first]: storeHeader(1, zlib.crc32(table), 1, 1 + 1 + 4, sectors),
        [first + 1]: table,
      };
      for (let k = 0; k < 4; k++) foreign[first + firstRel + k] = PATTERN;
      const rt = await kit.load('overlap', { selection: [], foreign });
      const { session } = rt;
      const mb = settingsMailbox(session, kit.plan);
      const usb = <T>(run: (dk: DkSession) => Promise<T>) => kit.usb(rt, run);
      const ev = await ringEvents(session, kit.plan, 'SETTINGS_LOAD');
      expect(ev.length).toBe(1);
      expect(ev[0]!.args[0], 'SETTINGS_LOAD state 4').toBe(4);
      expect(ev[0]!.flags & 1, 'an error event').toBe(1);
      expect(ev[0]!.args[3], "the store's status E_BAD_STORE").toBe(E_BAD_STORE >>> 0);
      const cap = (await usb((dk) => dk.capacity())).SETTINGS!;
      expect([cap.limit, cap.used], 'CAPACITY SETTINGS has limit 0 and nothing used').toEqual([0, 0]);
      expect(
        [await mb.put(ROUTE, byte(1)), (await mb.get(ROUTE))[0], await mb.remove(ROUTE)],
        'put, get and remove answer E_SET_OFF',
      ).toEqual([E_SET_OFF, E_SET_OFF, E_SET_OFF]);
      expect(await statusOf(() => usb(settingsRecord)), 'the SETTINGS command answers E_SET_OFF').toBe(E_SET_OFF);
      expect(
        await statusOf(() => usb((dk) => dk.list())),
        'the store is not used either (LIST answers E_NOSTORE)',
      ).toBe(E_NOSTORE);
      await advance(session, DELAY_MS + 500);
      const after = await kit.after('overlap', session.cardPath!);
      expect(await written(), 'nothing is written in the store range').toEqual([]);
      for (let k = 0; k < 4; k++)
        expect(
          sectorOf(after, first + firstRel + k).equals(PATTERN),
          `file sector ${k} is byte for byte as it was`,
        ).toBe(true);
    }, 600_000);

    test('oldcard: a card with the older header B at sectors - 3 boots, keeps its selection and repairs the header copy on an upload', async () => {
      const dir = path.join(kit.artifacts, 'newlayout');
      fs.mkdirSync(dir, { recursive: true });
      const newLayout = await buildCard(device, {
        dir,
        modules: [{ name: MODULE_PATH, file: moduleArtifact(device, MODULE) }],
        selection: [MODULE_PATH],
      });
      const hdrA = sectorOf(newLayout, first);
      // the card as an older runtime left it: header A and a copy of it at sectors - 3 (the old header B), nothing at today's header B
      const rt = await kit.load('oldcard', {
        modules: [MODULE],
        selection: [MODULE_PATH],
        foreign: { [headerB]: Buffer.alloc(SECTOR), [first + sectors - 3]: hdrA },
      });
      const { session } = rt;
      const usb = <T>(run: (dk: DkSession) => Promise<T>) => kit.usb(rt, run);
      const info = await bootInfo(session, kit.plan);
      expect(
        [info.results, info.safe, info.count, info.seq],
        'the module of the selection loads, no safe mode, the selection is kept',
      ).toEqual([[0], 0, 1, 1]);
      const ev = await ringEvents(session, kit.plan, 'SETTINGS_LOAD');
      const cap = await usb((dk) => dk.capacity());
      expect(ev.length).toBe(1);
      expect(ev[0]!.args[0], 'the settings store is on (no valid copy yet)').toBe(0);
      expect(cap.SETTINGS!.limit).toBe(8192);
      expect(
        (await usb((dk) => dk.list())).map((e) => e.path),
        'the store reports its file',
      ).toEqual([MODULE_PATH]);
      await settingsMailbox(session, kit.plan).put(ROUTE, byte(1));
      await advance(session, DELAY_MS + 500);
      expect(
        (await written()).filter((x) => x !== s.selA && x !== s.selB),
        "a settings write goes to copy A only (besides the selection record's boot counter)",
      ).toEqual([s.setA]);
      // an upload repairs the lagging header copy: header B is written at today's place, over free space
      await usb((dk) =>
        dk.put(
          't.bin',
          Uint8Array.from({ length: 768 }, (_, i) => i & 255),
        ),
      );
      const after = await kit.after('oldcard', session.cardPath!);
      const hb = sectorOf(after, headerB);
      const ha = sectorOf(after, first);
      expect(hb.subarray(0, 4).toString(), 'header B at the new place').toBe('DKST');
      expect(hb.subarray(24, 28).equals(ha.subarray(24, 28)), 'a header of the same seq as A').toBe(true);
      expect((await usb((dk) => dk.list())).map((e) => e.path).sort(), 'the store lists both files').toEqual(
        [MODULE_PATH, 't.bin'].sort(),
      );
      const d = await dumpJson(after);
      expect(d.store.files.map((f: { path: string }) => f.path).sort(), 'two files').toEqual(
        [MODULE_PATH, 't.bin'].sort(),
      );
      expect(d.selection.a.paths, 'the selection kept').toEqual([MODULE_PATH]);
      expect(d.settings.active, 'the key in settings copy A').toBe('A');
      kit.allowStoreWrites(await written());
    }, 600_000);
  });
}
