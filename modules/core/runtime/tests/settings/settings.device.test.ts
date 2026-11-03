import fs from 'node:fs';
import path from 'node:path';
import zlib from 'node:zlib';
import { hasCap, type DkSession } from '@digi/dk-session';
import { describe, expect, test } from 'vitest';
import { bootInfo, buildCard, cardInfo, E_NOSTORE, overlayArgs, statusOf } from '@digi/regression/runtime/card-kit.ts';
import { cardRig, RT, type CardSpec } from '@digi/regression/runtime/card-rig.ts';
import { cardStore, deviceProfile } from '@digi/regression/runtime/device-data.ts';
import { advance } from '@digi/regression/runtime/fx-live.ts';
import { DEVICES, FIRMWARE } from '@digi/regression/runtime/devices.ts';
import { moduleArtifact } from '@digi/regression/runtime/install.ts';
import { patchkit } from '@digi/regression/runtime/patchkit.ts';
import {
  decodeSettings,
  E_SET_KEY,
  E_SET_NOKEY,
  E_SET_OFF,
  encodeSettings,
  sectorOf,
  settingsMap,
  settingsMailbox,
  settingsRecord,
} from '@digi/regression/runtime/project-kit.ts';
import { RING_SYMBOLS, ringEvents } from '@digi/regression/runtime/runtime-menu.ts';

/**
 * The settings store of the runtime (docs/design/digikit-store.md "The settings record"; port of 6a5bb1ed:tools/patchkit/pk_digikit_settings.py).
 * A key put through the mailbox is written 2 s after the last change to the copy that is not current, and only there; a soft restart reads it back;
 * SETTINGS and CAPACITY report it; `patchkit store dump` decodes the card the guest left; a blank card is claimed first; a torn newer copy leaves the older one.
 * Route key persistence, error mapping with mocks and kit persistence are covered by fx-routes-persist, fault.test.ts and fx-kit-persist.
 */

const ROUTE = 'fx/route/test/dyn-fx-gain';
const SEND = 'fx/send/test/dyn-fx-gain';
const AMOUNTS = Buffer.from([0, 127, 64, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 100]);
const E_BAD_STORE = -2;
const DELAY_MS = 2000; // the runtime writes the record this long after the last change
const SECTOR = 512;
const MODULE = 'test/c-demo';
const MODULE_PATH = 'modules/c-demo.dkm';
const byte = (n: number) => Buffer.from([n]);
const text = (s: string) => Buffer.from(s);
const padded = (b: Buffer) => Buffer.concat([b, Buffer.alloc(SECTOR - b.length)]);

/** A pattern that is not a record, not a header and not blank. */
const PATTERN = Buffer.from(Array.from({ length: SECTOR }, (_, i) => (i * 7 + 3) & 0xff));

/** A store header of an older store (written before the settings area), checksummed. */
function storeHeader(count: number, tableCrc: number, seq: number, used: number, sectors: number): Buffer {
  const b = Buffer.alloc(SECTOR);
  b.write('DKST', 0);
  [3, count, 1, 1, tableCrc, seq, used, sectors].forEach((v, i) => b.writeUInt32BE(v, 4 + 4 * i));
  b.writeUInt32BE(zlib.crc32(b.subarray(0, 508)), 508);
  return b;
}

/** A copy with one bit pattern flipped so that its CRC fails. */
const torn = (b: Buffer) => {
  const x = Buffer.from(b);
  x[20] = x[20]! ^ 0x55;
  return x;
};

for (const device of DEVICES) {
  describe(`${device.toUpperCase()} ${FIRMWARE[device]} settings store`, () => {
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
    }, 900_000);

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
    }, 600_000);

    test('blank: the first settings write claims the blank area (the empty header pair) and then writes copy A', async () => {
      const rt = await kit.load('blank', {});
      const { session } = rt;
      const ci = await cardInfo(session, kit.plan);
      await settingsMailbox(session, kit.plan).put(ROUTE, byte(1));
      await advance(session, DELAY_MS + 500);
      const w = await written();
      const ci2 = await cardInfo(session, kit.plan);
      expect([ci.stateName, ci2.stateName, ci2.claims]).toEqual(['blank', 'ours', 1]);
      expect(w, 'the empty header pair, then copy A').toEqual([first, first + limit, s.setA].sort((a, b) => a - b));
      const d = await dumpJson(await kit.after('blank', session.cardPath!));
      expect(d.store.files, 'an empty store (header pair, no file)').toEqual([]);
      expect(d.settings.active).toBe('A');
      expect(d.settings.copies[0].entries[0].key, 'the key in copy A').toBe(ROUTE);
    }, 600_000);

    // cards with both copies set by hand
    const old = encodeSettings(4, [['k', text('old')]]);
    const newTorn = torn(encodeSettings(5, [['k', text('new')]]));
    const goodA = encodeSettings(9, [['k', text('A9')]]);
    const goodB = encodeSettings(3, [['k', text('B3')]]);
    const two = (a: Buffer, b: Buffer): CardSpec => ({
      selection: [],
      foreign: { [s.setA]: padded(a), [s.setB]: padded(b) },
    });
    const copies = [
      {
        name: 'torn',
        card: two(old, newTorn),
        what: 'copy A (B is torn)',
        load: [2, 0, 4],
        value: [3, 'old'],
        error: 1,
        next: 1,
        seq: 5,
      },
      {
        name: 'higher',
        card: two(goodA, goodB),
        what: 'copy A (seq 9 over 3)',
        load: [1, 0, 9],
        value: [2, 'A9'],
        error: 0,
        next: 1,
        seq: 10,
      },
      {
        name: 'both',
        card: two(torn(old), newTorn),
        what: 'no valid copy',
        load: [0, 2, 0],
        value: [E_SET_NOKEY, ''],
        error: 1,
        next: 0,
        seq: 1,
      },
    ] as const;
    test.each(copies)(
      'copies/$name: the boot reads $what and the next write goes to the other copy',
      async (want) => {
        const rt = await kit.load(`copies-${want.name}`, want.card);
        const { session } = rt;
        const mb = settingsMailbox(session, kit.plan);
        const ev = await ringEvents(session, kit.plan, 'SETTINGS_LOAD');
        expect(ev.length).toBe(1);
        expect(ev[0]!.args.slice(0, 3), 'state, copy, seq').toEqual(want.load);
        expect(ev[0]!.flags & 1, 'error event').toBe(want.error);
        const [n, v] = await mb.get('k');
        expect([n, v.toString()], 'get gives its value').toEqual(want.value);
        await mb.put('k2', text('new'));
        await advance(session, DELAY_MS + 500);
        const after = await kit.after(`copies-${want.name}`, session.cardPath!);
        const [target, keep] = want.next === 0 ? [s.setA, s.setB] : [s.setB, s.setA];
        const rec = decodeSettings(sectorOf(after, target));
        expect(await written(), `the next write goes to copy ${'AB'[want.next]}`).toEqual([target]);
        expect(rec?.seq).toBe(want.seq);
        expect(settingsMap(rec).k2).toBe(text('new').toString('hex'));
        expect(sectorOf(after, keep).equals(sectorOf(session.cardPath!, keep)), 'the other copy is untouched').toBe(
          true,
        );
      },
      600_000,
    );

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
