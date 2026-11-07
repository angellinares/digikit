import { createHash } from 'node:crypto';
import fs from 'node:fs';
import zlib from 'node:zlib';
import { fileHashHex } from '@digi/dk-session';
import type { PatchedSession } from '@digi/regression/support.ts';
import { expect, test } from 'vitest';
import {
  blob,
  bootInfoLoaded,
  cardInfo,
  overlayArgs,
  readSectors,
  statusOf,
  type SelRecord,
} from '@digi/regression/runtime/card-kit.ts';
import { CARD_SYMBOLS, cardRigs, RT } from '@digi/regression/runtime/card-rig.ts';
import { CARD_TIMEOUT } from '@digi/regression/runtime/debug-kit.ts';
import { cardStore, deviceProfile } from '@digi/regression/runtime/device-data.ts';
import { patchkit } from '@digi/regression/runtime/patchkit.ts';
import { encodeSettings } from '@digi/regression/runtime/project-kit.ts';
import { liveCheck as check } from '@digi/regression/runtime/artifacts.ts';
import { installDefaults, type DefaultOps } from '../../../../apps/web/src/firmware/default-modules-core.ts';
import { install, libraryModule } from '../../../../apps/web/src/modules/dktransfer.ts';

/**
 * The store's file hash (version 4: the +Drive content hash in the table's 32-byte slot) and what a store of another version is. Both devices.
 *  - A flipped byte on the card: VERIFY says another hash, the module with that byte is not loaded.
 *  - The flash scenario: a card with a real version 3 store (the old header, a table with a SHA-256 slot, a module), a selection and a settings
 *    record. The new runtime boots, LIST is empty, the first upload makes a version 4 store, a restart loads the module from the kept selection,
 *    and the workbench's default-module install (`installDefaults`, the same TypeScript code) ends with the module loaded.
 * The END with a wrong hash is in upload.device.test.ts ("a wrong file hash: E_HASH").
 */

const SECTOR = 512;
const MOD_PATH = 'modules/sine.dkm';
const sectorsOf = (n: number) => Math.ceil(n / SECTOR);
const padded = (b: Buffer) => Buffer.concat([b, Buffer.alloc(sectorsOf(b.length) * SECTOR - b.length)]);

/** The bytes the previous runtime left for one module: header A and B (version 3), a table of one entry with the file's SHA-256, the file at sector 2. */
function version3Store(device: 'dt2' | 'dn2', file: Buffer): Record<number, Buffer> {
  const { first, end } = cardStore(device);
  const headerB = deviceProfile(device).card.store.header_b_sector;
  const entry = Buffer.alloc(256);
  entry.write(MOD_PATH, 0);
  entry.writeUInt32BE(2, 160);
  entry.writeUInt32BE(file.length, 164);
  createHash('sha256').update(file).digest().copy(entry, 168);
  const table = padded(entry);
  const head = Buffer.alloc(SECTOR);
  head.write('DKST', 0);
  [3, 1, 1, 1, zlib.crc32(table), 1, 2 + sectorsOf(file.length), end - first].forEach((v, i) =>
    head.writeUInt32BE(v >>> 0, 4 + 4 * i),
  );
  head.writeUInt32BE(zlib.crc32(head.subarray(0, 508)), 508);
  return { [first]: head, [headerB]: head, [first + 1]: table, [first + 2]: padded(file) };
}

/** The sectors (relative to the store) of the old store's bytes that `card` holds differently now. */
const oldSectors = (v3: Record<number, Buffer>, first: number, card: string): number[] =>
  Object.entries(v3).flatMap(([s, b]) =>
    Array.from({ length: b.length / SECTOR }, (_, k) => k).filter(
      (k) => !readSectors(card, Number(s) + k, 1).equals(b.subarray(k * SECTOR, (k + 1) * SECTOR)),
    ).map((k) => Number(s) + k - first),
  );


cardRigs(
  'store format 4',
  { uploads: true, relaxFs: true, roots: RT, symbols: CARD_SYMBOLS },
  (h) => {
    const store = cardStore(h.device);
    const headerB = deviceProfile(h.device).card.store.header_b_sector;
    /** The modules the runtime reports loaded (protocol calls run on the paused daemon). */
    const names = async (rt: PatchedSession) => (await h.usb(rt, (dk) => dk.modules())).map((m) => m.name);
    const sine = h.mod('test/sine');
    const bytes = () => fs.readFileSync(sine.file);
    const dump = async (card: string) =>
      JSON.parse((await patchkit(['store', 'dump', card, ...overlayArgs(card), '--device', h.device, '--json'])).stdout);

    test(
      'a flipped byte on the card: VERIFY reports it and the module is not loaded',
      async () => {
        const good = bytes();
        let rt: PatchedSession = await h.load('hash-good', { modules: [sine], selection: [MOD_PATH] });
        expect(await bootInfoLoaded(rt.session, h.plan), 'the builder\'s hash is the runtime\'s: the module loads').toMatchObject({
          count: 1,
          loaded: 1,
          safe: 0,
        });
        check('VERIFY of the module equals the host hash', (await h.usb(rt, (dk) => dk.verify(MOD_PATH))) === fileHashHex(good));
        // a second file, as a sample is: uploaded by the host over the protocol, verified, listed
        const sample = blob(5000, 9);
        expect(await statusOf(() => h.usb(rt, (dk) => dk.put('samples/s', sample)))).toBe(0);
        check('VERIFY of an uploaded file equals the host hash', (await h.usb(rt, (dk) => dk.verify('samples/s'))) === fileHashHex(sample));
        expect((await h.usb(rt, (dk) => dk.list())).find((e) => e.path === 'samples/s')?.hash).toBe(fileHashHex(sample));
        const after = await h.after('good');
        const ls = (await h.ls('good')).entries;
        const abs = (p: string) => store.first + ls.find((e) => e.path === p)!.firstSector;

        // flip one byte of each file on the card
        const flipped = (p: string, at: number) => {
          const s = Buffer.from(readSectors(after, abs(p), 1));
          s[at] = s[at]! ^ 0x10;
          return s;
        };
        rt = await h.load('hash-bad', {
          base: after,
          foreign: { [abs(MOD_PATH)]: flipped(MOD_PATH, 300), [abs('samples/s')]: flipped('samples/s', 100) },
        });
        const badModule = Buffer.from(good);
        badModule.writeUInt8(badModule.readUInt8(300) ^ 0x10, 300);
        const badSample = Buffer.from(sample);
        badSample.writeUInt8(badSample.readUInt8(100) ^ 0x10, 100);
        check('VERIFY of the module says what the card holds now', (await h.usb(rt, (dk) => dk.verify(MOD_PATH))) === fileHashHex(badModule));
        check('VERIFY of the sample says what the card holds now', (await h.usb(rt, (dk) => dk.verify('samples/s'))) === fileHashHex(badSample));
        check('and that is not the host hash', fileHashHex(badModule) !== fileHashHex(good));
        const info = await bootInfoLoaded(rt.session, h.plan);
        expect(info, 'the selected module is not loaded').toMatchObject({ count: 1, loaded: 0 });
        expect(info.results, 'its load fails the hash check (E_SHA)').toEqual([-4]);
        expect(await names(rt), 'no module is registered').toEqual([]);
      },
      CARD_TIMEOUT,
    );

    test(
      'flash scenario: a version 3 store counts as empty, the first upload makes version 4, the kept selection loads the module',
      async () => {
        const file = bytes();
        const settings = Buffer.concat([
          encodeSettings(2, [['fx/route/test/x', Buffer.from([1])]]),
        ]);
        const v3 = version3Store(h.device, file);
        const settingsCopy = Buffer.concat([settings, Buffer.alloc(SECTOR - settings.length)]);
        const facts: Record<string, unknown> = {};

        // boot: it goes on, the old store is no store
        let rt: PatchedSession = await h.load('v3-flash', {
          selection: [MOD_PATH],
          foreign: { ...v3, [store.setA]: settingsCopy },
        });
        const boot = await bootInfoLoaded(rt.session, h.plan);
        facts.boot = boot;
        expect(boot, 'the boot goes on, nothing is loaded, no safe mode').toMatchObject({ safe: 0, loaded: 0, uiReached: 1 });
        expect((await cardInfo(rt.session, h.plan)).stateName, 'the card is ours').toBe('ours');
        expect(await h.usb(rt, (dk) => dk.list()), 'LIST is empty').toEqual([]);
        expect(await names(rt)).toEqual([]);
        const quiet = await h.after('v3-boot');
        facts.selectionAfterBoot = (await h.records()).map((r: SelRecord | null) => r && r.paths);
        const d0 = await dump(quiet);
        facts.settingsAfterBoot = d0.settings ?? null;
        facts.storeAfterBoot = d0.store ?? null;
        const changed = oldSectors(v3, store.first, quiet);
        check('the old bytes are untouched by a boot', changed.length === 0, `changed store sectors ${changed.join()}`);

        // the first upload makes a version 4 store
        expect(await statusOf(() => h.usb(rt, (dk) => dk.put(MOD_PATH, file))), 'the upload works').toBe(0);
        const listed = await h.usb(rt, (dk) => dk.list());
        expect(listed.map((e) => [e.path, e.size, e.hash])).toEqual([[MOD_PATH, file.length, fileHashHex(file)]]);
        let view = await h.view('v4-first');
        expect(view.head, 'the card holds a version 4 store').toContain('version=4');
        const became = (card: string) => oldSectors(v3, store.first, card);
        const afterFirst = await h.after('v4-first');
        facts.oldBytesAfterFirstUpload = became(afterFirst);
        expect(readSectors(afterFirst, store.first, 1).readUInt32BE(4), 'header A is version 4').toBe(4);
        expect(readSectors(afterFirst, headerB, 1).readUInt32BE(4), 'header B is version 4').toBe(4);

        // a restart: the kept selection loads the module
        rt = await h.restart(rt);
        facts.loadedAfterRestart = await names(rt);
        facts.selectionAfterRestart = (await h.records()).map((r: SelRecord | null) => r && r.paths);
        expect(await names(rt), 'the kept selection loads the uploaded module').toEqual(['test/sine']);

        // again: a second file, and what the old bytes are then
        expect(await statusOf(() => h.usb(rt, (dk) => dk.put('modules/b.dkm', blob(3000, 5))))).toBe(0);
        facts.oldBytesAfterSecondUpload = became(await h.after('v4-second'));
        const d1 = await dump(await h.after('v4-dump'));
        facts.settingsAfterUploads = d1.settings ?? null;
        fs.writeFileSync(`${h.artifacts}/store-format-facts-${h.device}.json`, JSON.stringify(facts, null, 1));
        console.log(`store-format facts ${h.device}: ${JSON.stringify(facts)}`);
      },
      CARD_TIMEOUT * 2,
    );

    test(
      'default modules on a card with a version 3 store: installDefaults ends with the module loaded',
      async () => {
        const file = bytes();
        let rt: PatchedSession = await h.load('v3-defaults', { selection: [], foreign: version3Store(h.device, file) });
        const lib = (await libraryModule(file))!;
        const library = [lib];
        const id = lib.id;
        const ops: DefaultOps = {
          pathOf: (i) => library.find((m) => m.id === i)?.path,
          waitForRuntime: async () => void (await h.usb(rt, (dk) => dk.info())),
          upload: async (i: string) =>
            (await h.usb(rt, (dk) => install(dk, [i], library))).uploaded.includes(i) ? 'uploaded' : 'same',
          loaded: async () => (await h.usb(rt, (dk) => dk.modules())).map((m) => m.name),
          select: (paths) => h.usb(rt, (dk) => dk.select(paths)),
          restart: async () => {
            rt = await h.restart(rt);
            return names(rt);
          },
        };
        const lines = await installDefaults(ops, [id]);
        expect(lines.at(-1), 'installDefaults ends with the module enabled').toContain(`enabled ${id}`);
        expect(await names(rt)).toEqual([id]);
      },
      CARD_TIMEOUT * 2,
    );
  },
);
