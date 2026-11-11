import { execFile } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { promisify } from 'node:util';
import { be32, fileHash, type DkSession } from '@digi/dk-session';
import { repoRoot } from '@digi/test-support';
import { expect, test } from 'vitest';
import { overlayArgs, plusdriveBinary, pokeKnob, pokeWord, readSectors } from '@digi/regression/runtime/card-kit.ts';
import { cardRigs, RT, type CardRig } from '@digi/regression/runtime/card-rig.ts';
import { advance, word } from '@digi/regression/runtime/fx-live.ts';
import { overlayFile, overlayRecords } from '@digi/regression/runtime/write-safety.ts';

/**
 * Writes to the stock +Drive through core/plusdrive-write (docs/plans/usb-fast.md 3.1, finding 120), on the Digitakt II 1.17. The fixture
 * test/file-write starts an I/O job that writes a file of a known pattern through file_* on DK_FILES_SAMPLES. The card is read back with
 * `plusdrive fs ls` / `fs cat` through the overlay of the guest's writes: the path filesystem as the firmware left it.
 */

const exec = promisify(execFile);
const CMD = 0x24;
const E_EXISTS = -18;
const E_FILE_IN_USE = -71;
const WRITE = 1;
const ABORT = 2;
const DELETE = 6;
const BIG_CALL = 10;
const REPLACE = 0x40;
const SAMPLES = 0x80 | 0x20; // the samples root; the expected hash covers the data and the 16 zero bytes
const SAMPLES_ONLY = 0x80;
const STAGING = '.dkstage';
const pattern = (n: number) => Buffer.from(Uint8Array.from({ length: n }, (_, i) => (i * 7 + (i >> 8)) & 255));
const cstr = (s: string) => Buffer.concat([Buffer.from(s), Buffer.from([0])]);
const withTail = (b: Buffer) => Buffer.concat([b, Buffer.alloc(16)]);
const slot4 = (b: Buffer) => Buffer.from(fileHash(withTail(b))).subarray(0, 4).toString('hex');

interface Entry {
  size: number;
  hash: string;
}

async function drive(card: string): Promise<Map<string, Entry>> {
  const { stdout } = await exec(plusdriveBinary(), ['fs', 'ls', card, ...overlayArgs(card)], { cwd: repoRoot(), maxBuffer: 1 << 26 });
  const out = new Map<string, Entry>();
  for (const line of stdout.trim().split('\n')) {
    const [p, size, hash] = line.split('\t') as [string, string, string];
    if (p.endsWith('/')) out.set(p, { size: 0, hash: '' });
    else out.set(p, { size: Number(size), hash });
  }
  return out;
}

async function cat(card: string, p: string): Promise<Buffer> {
  const r = await exec(plusdriveBinary(), ['fs', 'cat', card, p, ...overlayArgs(card)], {
    cwd: repoRoot(),
    encoding: 'buffer',
    maxBuffer: 1 << 28,
  });
  return r.stdout;
}

/** Files that are not part of the written file's story: everything but the destination and the staging folder. */
const others = (m: Map<string, Entry>, except: string[]) =>
  [...m].filter(([p]) => !except.includes(p) && !p.startsWith(`${STAGING}/`) && p !== `${STAGING}/`);
const noStagingFolder = (m: Map<string, Entry>) => !m.has(`${STAGING}/`);
const staged = (m: Map<string, Entry>) => [...m.keys()].filter((p) => p.startsWith(`${STAGING}/`) && p !== `${STAGING}/`);

cardRigs('+Drive writes through core/plusdrive-write', { devices: ['dt2'] , dsp: 'zero', roots: [...RT, 'core/plusdrive-write'], symbols: ['DKRT_MAILBOX', 'DKRT_SESSION_IDLE', 'DKRT_PD_CUT'], project: true, uploads: true, relaxFs: true }, (rig: CardRig) => {
  const SEEDS: Record<string, Buffer> = {
    'keep.bin': pattern(300),
    'Samples/old.bin': pattern(5000),
    'Samples/Sub/deep.bin': pattern(1234),
  };
  const bases = new Map<string, string>();
  const baseCard = async (extra: Record<string, Buffer> = {}, name = 'pd-base') => {
    const known = bases.get(name);
    if (known) return known;
    const mod = rig.mod('test/file-write');
    const drive_ = Object.entries({ ...SEEDS, ...extra }).map(([name, bytes]) => {
      const file = path.join(rig.artifacts, `seed-${name.replace(/[/.]/g, '_')}`);
      fs.writeFileSync(file, bytes);
      return { name, file };
    });
    const card = await rig.card({ name, modules: [mod], drive: drive_, selection: [mod.name] });
    bases.set(name, card);
    return card;
  };

  const start = async (dk: DkSession, kind: number, size: number, p: string) =>
    (await dk.request(CMD, Buffer.concat([Buffer.from([kind]), be32(size), cstr(p)]))).status;
  /** Run the emulator until the job ends; the job's fields. */
  const finish = async (dk: DkSession) => {
    for (let i = 0; i < 400; i++) {
      await advance(rig.session, 100);
      const b = Buffer.from((await dk.request(CMD + 1)).body);
      if (b[0] === 2) return { create: b.readInt32BE(1), close: b.readInt32BE(5), abort: b.readInt32BE(9), job: b.readInt32BE(13), devHash: b.subarray(21, 25).toString('hex'), written: b.readUInt32BE(29) };
    }
    throw new Error('the job did not end');
  };
  const run = async (dk: DkSession, kind: number, size: number, p: string) => {
    expect(await start(dk, kind, size, p), 'the job is queued').toBe(0);
    return finish(dk);
  };
  const snapshot = async (tag: string) => {
    const card = await rig.after(tag);
    return { card, files: await drive(card) };
  };
  const same = (before: Map<string, Entry>, after: Map<string, Entry>, except: string[]) =>
    expect(others(after, except), 'no other file changed').toEqual(others(before, except));

  test('a write lands at its destination with the firmware trailer and hash, creates the folders, leaves no staging file; E_EXISTS without REPLACE; REPLACE swaps; an abort leaves nothing; a delete removes the file', async () => {
    const dk = await rig.pausedBoot(await baseCard());
    await advance(rig.session, 2000); // the boot recovery of the staging folder
    const before = (await snapshot('pd0')).files;
    expect(before.get('Samples/old.bin')?.size, 'the seed file is there').toBeGreaterThan(0);

    const big = pattern(70000); // one 64 KiB file_write call (two 32 KiB writes) and a tail
    const a = await run(dk, BIG_CALL | SAMPLES, big.length, 'Samples/Kits/new.bin');
    expect([a.create > 0, a.close, a.job], 'created, closed, finished').toEqual([true, 0, 0]);
    expect(a.devHash, 'file_close gives the hash of the data and 16 zeros').toBe(slot4(big));
    let s = await snapshot('pd1');
    expect(s.files.get('Samples/Kits/new.bin'), 'size N+16, the hash of data + 16 zeros').toEqual({ size: big.length + 16, hash: slot4(big) });
    expect((await cat(s.card, 'Samples/Kits/new.bin')).subarray(0, big.length), 'the bytes').toEqual(big);
    expect(staged(s.files), 'the staging folder is empty').toEqual([]);
    // the folder stays for the upload session (a second file reuses it); the runtime removes the empty folder 2 s after the last session
    expect(noStagingFolder(s.files), 'the staging folder is still there right after the session').toBe(false);
    await advance(rig.session, 3000);
    s = await snapshot('pd1b');
    expect(noStagingFolder(s.files), 'and gone after the idle time: it exists only during an upload session').toBe(true);
    same(before, s.files, ['Samples/Kits/new.bin', 'Samples/Kits/']);

    // no REPLACE: the existing file is E_EXISTS before any data is written
    const ex = await run(dk, WRITE | SAMPLES, 777, 'Samples/old.bin');
    expect([ex.create, ex.job]).toEqual([E_EXISTS, E_EXISTS]);
    s = await snapshot('pd2');
    expect(s.files.get('Samples/old.bin'), 'the old file is as it was').toEqual(before.get('Samples/old.bin'));
    expect(staged(s.files)).toEqual([]);

    // REPLACE
    const fresh = pattern(9000);
    const r = await run(dk, WRITE | REPLACE | SAMPLES, fresh.length, 'Samples/old.bin');
    expect([r.create > 0, r.close, r.job, r.devHash]).toEqual([true, 0, 0, slot4(fresh)]);
    s = await snapshot('pd3');
    expect(s.files.get('Samples/old.bin')).toEqual({ size: fresh.length + 16, hash: slot4(fresh) });
    expect((await cat(s.card, 'Samples/old.bin')).subarray(0, fresh.length)).toEqual(fresh);
    expect(staged(s.files)).toEqual([]);
    same(before, s.files, ['Samples/old.bin', 'Samples/Kits/new.bin', 'Samples/Kits/']);

    // an abort (one chunk written, then file_abort) leaves nothing, for a new path and for a replace
    const ab = await run(dk, ABORT | SAMPLES, 20000, 'Samples/aborted.bin');
    expect([ab.abort, ab.job, ab.written]).toEqual([0, 0, 4096]);
    const abr = await run(dk, ABORT | REPLACE | SAMPLES, 20000, 'Samples/old.bin');
    expect([abr.abort, abr.job]).toEqual([0, 0]);
    s = await snapshot('pd4');
    expect(s.files.has('Samples/aborted.bin'), 'no file at the aborted path').toBe(false);
    expect(s.files.get('Samples/old.bin'), 'the replaced file is untouched by the abort').toEqual({ size: fresh.length + 16, hash: slot4(fresh) });
    expect(staged(s.files)).toEqual([]);

    // a wrong expected hash is E_HASH and leaves nothing
    const bad = await run(dk, 5 | SAMPLES, 3000, 'Samples/bad.bin'); // WRITE_BAD_HASH
    expect([bad.close, bad.job]).toEqual([-56, -56]);
    s = await snapshot('pd5');
    expect(s.files.has('Samples/bad.bin')).toBe(false);
    expect(staged(s.files)).toEqual([]);

    // a folder at the destination, or a file where a folder of the path should be, is E_PATH
    const folder = await run(dk, WRITE | SAMPLES, 500, 'Samples/Sub');
    expect([folder.create, folder.job], 'a folder at the destination').toEqual([-51, -51]);
    const under = await run(dk, WRITE | SAMPLES, 500, 'keep.bin/inside.bin');
    expect([under.create, under.job], 'a file above the destination').toEqual([-51, -51]);
    const factory = await run(dk, WRITE | SAMPLES, 500, 'factory/x.bin');
    expect([factory.create, factory.job], 'inside the firmware ROM tree /factory').toEqual([-51, -51]);
    const inStaging = await run(dk, WRITE | SAMPLES, 500, `${STAGING}/s00000000`);
    expect([inStaging.create, inStaging.job], 'inside the staging folder').toEqual([-51, -51]);

    // a delete removes the file; a missing one is E_NOFILE
    const del = await run(dk, DELETE | SAMPLES_ONLY, 0, 'Samples/Kits/new.bin');
    expect([del.close, del.job]).toEqual([0, 0]);
    const gone = await run(dk, DELETE | SAMPLES_ONLY, 0, 'Samples/Kits/new.bin');
    expect(gone.job, 'E_NOFILE').toBe(-59);
    s = await snapshot('pd6');
    expect(s.files.has('Samples/Kits/new.bin')).toBe(false);
    same(before, s.files, ['Samples/old.bin', 'Samples/Kits/new.bin', 'Samples/Kits/']);
    await rig.session.expectNoFault('+Drive writes');
  }, 1_800_000);

  // The close hash (finding 120): the adapter hashes the card's pages with its own code and runs the rest of the firmware's close hash
  // (hash page, table rebuild, inode word, event, write-back). DKRT_PD_CUT = 0x80 closes with the firmware's own hash instead. Both are run
  // from the same card and must leave the same card sectors of the filesystem (inodes, hash pages, bitmap, data) and the same RAM hash table.
  const FS_FIRST = 0x5d8000; // the superblock "SFke"; the store and the settings are below
  const HASH_PAGES = [0x5ee980, 0x5ee980 + 44 * 0x40]; // the 44 hash pages
  const TABLE = 0x4067d8c0; // the RAM hash table: count, then (hash, id) pairs
  const fsWrites = (card: string) => {
    const m = new Map<number, string>();
    for (const r of overlayRecords(fs.readFileSync(overlayFile(card)))) if (r.sector >= FS_FIRST) m.set(r.sector, Buffer.concat([r.mask, r.data]).toString('hex'));
    return m;
  };
  const hashTable = async () => {
    const n = await word(rig.session, TABLE);
    const out: number[] = [n];
    for (let i = 0; i < n * 2; i++) out.push(await word(rig.session, TABLE + 4 + 4 * i));
    return out;
  };
  // The inode records of the path filesystem: 128-byte records, 256 per 32 KiB page, from sector 0x5d8180 (packages/emulator/plusdrive/src/reader.rs).
  const REC_FIRST = 0x5d8180;
  const REC_SECTORS = 0x16000;
  const REC = 0x80;
  const inodes = (card: string) => readSectors(card, REC_FIRST, REC_SECTORS);
  const at = (area: Buffer, id: number) => area.subarray((Math.floor(id / 256) * 0x40) * 512 + (id % 256) * REC, (Math.floor(id / 256) * 0x40) * 512 + (id % 256) * REC + REC);
  const live = (area: Buffer, test: (r: Buffer) => boolean) => {
    const ids: number[] = [];
    for (let id = 2; id < (REC_SECTORS / 0x40) * 256; id++) {
      const r = at(area, id);
      if (r.readUInt16BE(2) > 0 && test(r)) ids.push(id);
    }
    return ids;
  };
  const fileOfSize = (area: Buffer, size: number) => live(area, (r) => (r[0]! & 1) === 0 && r.readUInt32BE(4) === size);
  /** The extents of a record: [logical page, pages, physical page]; the count only above the 8 inline extents (their index pages are not read). */
  const extentsOf = (area: Buffer, id: number) => {
    const r = at(area, id);
    const count = r.readUInt16BE(0x1e);
    const list: number[][] = [];
    if (count <= 8) for (let i = 0; i < count; i++) list.push([r.readUInt32BE(0x20 + 12 * i), r.readUInt32BE(0x24 + 12 * i), r.readUInt32BE(0x28 + 12 * i)]);
    return { count, list };
  };
  const bothHashes = async (knob: number) => {
    const dk = await rig.pausedBoot(await baseCard());
    await advance(rig.session, 2000);
    const initial = await hashTable();
    await pokeKnob(rig.session, rig.plan, 'DKRT_PD_CUT', knob);
    const a = pattern(70000); // two pages and a tail
    const b = pattern(5000); // one partial page; the second file's table holds both
    const c = pattern(32752); // data + 16 zeros is exactly one page
    const r: string[] = [];
    for (const [d, p] of [[a, 'Samples/Kits/a.bin'], [b, 'Samples/b.bin'], [c, 'Samples/c.bin']] as const) {
      const x = await run(dk, WRITE | SAMPLES, d.length, p);
      expect([x.close, x.job, x.devHash], `${p} closed with the hash of data + 16 zeros`).toEqual([0, 0, slot4(d)]);
      r.push(x.devHash);
    }
    const card = await rig.after(`pdh${knob}`);
    const files = await drive(card);
    for (const [d, p] of [[a, 'Samples/Kits/a.bin'], [b, 'Samples/b.bin'], [c, 'Samples/c.bin']] as const) {
      expect(files.get(p), p).toEqual({ size: d.length + 16, hash: slot4(d) });
      expect((await cat(card, p)).subarray(0, d.length), `${p} bytes`).toEqual(d);
    }
    // the card as the three closes left it, before the end of the batch (the staging folder is still there)
    const writes = fsWrites(card);
    const during = await hashTable();
    await advance(rig.session, IDLE_MS); // the runtime's idle point: the rebuild of the table (and the removal of the staging folder)
    return { writes, initial, during, table: await hashTable(), hashes: [a, b, c].map((d) => parseInt(slot4(d), 16)) };
  };
  // The runtime calls the adapter's recovery after 2 s without a write session (PD_STAGE_IDLE): the fast close rebuilds the RAM hash table then.
  const IDLE_MS = 4000;
  test('the close hash with the adapter\'s own code leaves the card and the RAM hash table as the firmware\'s close hash does', async () => {
    const fast = await bothHashes(0);
    const slow = await bothHashes(0x80);
    expect(fast.table[0], 'the hash table has entries').toBeGreaterThan(3);
    expect(slow.during, 'the firmware close rebuilds at every file').toEqual(slow.table);
    expect(fast.during[0], 'the batch has not rebuilt the table: it lacks the three new files').toBe(fast.table[0]! - 3);
    expect(fast.during, 'before the idle point the table is the one the boot built').toEqual(fast.initial);
    // a lookup by hash (the firmware's FUN_4015ac28 searches these pairs): the new files are found after the rebuild only
    const hashWords = (t: number[]) => t.slice(1).filter((_, i) => i % 2 === 0).map((w) => w >>> 0);
    for (const h of fast.hashes) {
      expect(hashWords(fast.during).some((w) => (w >>> 1) === (h >>> 1)), `hash ${h.toString(16)} is not in the table yet`).toBe(false);
      expect(hashWords(fast.table).some((w) => (w >>> 1) === (h >>> 1)), `hash ${h.toString(16)} is in the table after the rebuild`).toBe(true);
    }
    expect(fast.table, 'the RAM hash table after the rebuild at the idle point').toEqual(slow.table);
    expect([...fast.writes.keys()].sort((x, y) => x - y), 'the same sectors were written').toEqual([...slow.writes.keys()].sort((x, y) => x - y));
    expect(fast.writes.size, 'the card was written').toBeGreaterThan(20);
    for (const [sector, data] of fast.writes) expect(data, `sector 0x${sector.toString(16)}`).toBe(slow.writes.get(sector));
    await rig.session.expectNoFault('close hash');
  }, 1_800_000);

  test('an abort in the middle of a batch rebuilds the RAM hash table at once', async () => {
    const dk = await rig.pausedBoot(await baseCard());
    await advance(rig.session, 2000);
    const initial = await hashTable();
    const a = pattern(40000);
    const x = await run(dk, WRITE | SAMPLES, a.length, 'Samples/batch.bin');
    expect([x.close, x.job]).toEqual([0, 0]);
    expect(await hashTable(), 'the close left the table as it was').toEqual(initial);
    const ab = await run(dk, ABORT | SAMPLES, 20000, 'Samples/aborted.bin'); // a second file of the batch is dropped before its close
    expect([ab.abort, ab.job], 'the session was aborted').toEqual([0, 0]);
    const t = await hashTable();
    expect(t[0], 'the abort rebuilt the table: it holds the first file').toBe(initial[0]! + 1);
    expect(t.slice(1).filter((_, i) => i % 2 === 0).some((w) => (w >>> 1) === (parseInt(slot4(a), 16) >>> 1))).toBe(true);
    await rig.session.expectNoFault('abort rebuild');
  }, 1_800_000);

  // The only part of the fast close that differs from the stock read is the page to card mapping (file_page_find), and a file of one extent does
  // not test it. The adapter reserves the whole file in one call; the firmware's allocator (FUN_40155974, FUN_4015583c) takes the first free run of
  // that length, else the longest runs it finds, one extent each. So the free space is fragmented in RAM before the write: in the page occupancy
  // bitmap (0x46f39ca0, one bit per 32 KiB page, set = used) every even page from the first one after the seed files is marked used, so that every
  // free run is one page long. Both variants get the same bitmap.
  const BITMAP = 0x46f39ca0;
  const FRAG_FIRST_WORD = 83; // pages 2656 and up; the allocator starts at page 2682
  const FRAG_LAST_WORD = 0x5004; // the allocator scans this many words (FUN_4015583c)
  const FRAG_USED_WORDS = 100; // pages below 3200 may hold files: their bits are kept
  const FRAG_PAGES = 20; // more pieces than the 8 inline extents, which puts the extents into an index page
  const fragBig = async (knob: number) => {
    const dk = await rig.pausedBoot(await baseCard());
    await advance(rig.session, 2000);
    for (let w = FRAG_FIRST_WORD; w < FRAG_LAST_WORD; w++) {
      const kept = w < FRAG_USED_WORDS ? await word(rig.session, BITMAP + 4 * w) : 0;
      await pokeWord(rig.session, BITMAP + 4 * w, (kept | 0x55555555) >>> 0);
    }
    await pokeKnob(rig.session, rig.plan, 'DKRT_PD_CUT', knob);
    const big = pattern(FRAG_PAGES * 32768 - 16 - 700);
    const x = await run(dk, WRITE | SAMPLES, big.length, 'Samples/Frag/big.bin');
    expect([x.close, x.job, x.devHash], 'closed with the hash of data + 16 zeros').toEqual([0, 0, slot4(big)]);
    const out = await rig.after(`pdg${knob}`);
    expect((await cat(out, 'Samples/Frag/big.bin')).subarray(0, big.length), 'bytes').toEqual(big);
    const area = inodes(out);
    const ids = fileOfSize(area, big.length + 16);
    expect(ids.length, 'one record of that size').toBe(1);
    const writes = fsWrites(out);
    await advance(rig.session, IDLE_MS); // the batch ends: the table is rebuilt
    return { card: out, writes, table: await hashTable(), extents: extentsOf(area, ids[0]!) };
  };
  test('the close hash on a fragmented file (several extents) leaves the card and the RAM hash table as the firmware\'s close hash does', async () => {
    const fast = await fragBig(0);
    const slow = await fragBig(0x80);
    // the file is fragmented: the firmware's own extent count in the inode, read from the card
    console.log(`fragmented file: ${fast.extents.count} extents`);
    expect(fast.extents.count, 'more extents than the 8 inline ones').toBeGreaterThan(8);
    expect(slow.extents.count, 'the same layout through the firmware close').toBe(fast.extents.count);
    expect(fast.extents.list, 'the same extents').toEqual(slow.extents.list);
    expect(fast.table, 'the RAM hash table after the rebuild').toEqual(slow.table);
    expect([...fast.writes.keys()].sort((x, y) => x - y), 'the same sectors were written').toEqual([...slow.writes.keys()].sort((x, y) => x - y));
    for (const [sector, data] of fast.writes) expect(data, `sector 0x${sector.toString(16)}`).toBe(slow.writes.get(sector));
    await rig.session.expectNoFault('fragmented close hash');
  }, 3_600_000);

  test('a failure after the hash page was written puts the hash word back: nothing points at the deleted inode', async () => {
    const dk = await rig.pausedBoot(await baseCard());
    await advance(rig.session, 2000);
    const before = await snapshot('pdw0');
    const tableBefore = await hashTable();
    await pokeKnob(rig.session, rig.plan, 'DKRT_PD_CUT', 9); // the close fails right after the hash page write
    const r = await run(dk, WRITE | SAMPLES, 40000, 'Samples/never.bin');
    expect(r.job, 'write error').toBe(-57);
    const after = await snapshot('pdw1');
    expect(fsWrites(after.card).has(HASH_PAGES[0]!), 'the hash page was written (and written back)').toBe(true);
    const pages = (card: string) => readSectors(card, HASH_PAGES[0]!, HASH_PAGES[1]! - HASH_PAGES[0]!);
    expect(pages(after.card).equals(pages(before.card)), 'every hash word is as it was').toBe(true);
    expect(await hashTable(), 'the RAM hash table was not rebuilt').toEqual(tableBefore);
    expect(after.files.has('Samples/never.bin')).toBe(false);
    expect(staged(after.files)).toEqual([]);
    same(before.files, after.files, []);
    await pokeKnob(rig.session, rig.plan, 'DKRT_PD_CUT', 0);
    const ok = await run(dk, WRITE | SAMPLES, 3000, 'Samples/good.bin');
    expect([ok.close, ok.job], 'the next file is accepted').toEqual([0, 0]);
    await rig.session.expectNoFault('hash word restore');
  }, 1_800_000);

  test('a wrong expected hash is refused and leaves the card\'s hash pages and the RAM hash table as they were', async () => {
    const dk = await rig.pausedBoot(await baseCard());
    await advance(rig.session, 2000);
    const before = await snapshot('pdm0');
    const tableBefore = await hashTable();
    const bad = await run(dk, 5 | SAMPLES, 40000, 'Samples/bad.bin'); // WRITE_BAD_HASH: two pages and a tail
    expect([bad.close, bad.job], 'E_HASH').toEqual([-56, -56]);
    const after = await snapshot('pdm1');
    expect(after.files.has('Samples/bad.bin')).toBe(false);
    expect(staged(after.files), 'staging recovered').toEqual([]);
    same(before.files, after.files, []);
    expect(await hashTable(), 'the RAM hash table is untouched').toEqual(tableBefore);
    // the delete of the staged file may write a hash page back, so the test compares the content of the 44 hash pages, not the writes
    const pages = (card: string) => readSectors(card, HASH_PAGES[0]!, HASH_PAGES[1]! - HASH_PAGES[0]!);
    expect(pages(after.card).equals(pages(before.card)), 'the hash pages on the card hold what they held').toBe(true);
    // the inodes of the seed files and of their folders are unchanged; only the staged file's own records and the staging folder's change
    const a0 = inodes(before.card);
    const a1 = inodes(after.card);
    const seedIds = [5000, 1234, 300].flatMap((n) => [...fileOfSize(a0, n), ...fileOfSize(a0, n + 16)]); // old.bin, deep.bin, keep.bin
    expect(seedIds.length, 'the seed files were found').toBe(3);
    const root = at(a0, seedIds[2]!).readUInt32BE(8); // keep.bin's folder; the staging folder entry may touch it
    const folders = new Set(seedIds.map((id) => at(a0, id).readUInt32BE(8)));
    folders.delete(root);
    for (const id of [...seedIds, ...folders]) expect(at(a1, id).equals(at(a0, id)), `inode ${id} (a seed file or the Samples/Sub folder) is unchanged`).toBe(true);
    let changed = 0;
    for (let id = 2; id < (REC_SECTORS / 0x40) * 256; id++) if (!at(a1, id).equals(at(a0, id))) changed++;
    expect(changed, 'only the staged file and the staging folder (and the root entry for it) were touched').toBeLessThanOrEqual(4);
    // the next good file is accepted
    const ok = await run(dk, WRITE | SAMPLES, 3000, 'Samples/good.bin');
    expect([ok.close, ok.job]).toEqual([0, 0]);
    await rig.session.expectNoFault('wrong hash');
  }, 1_800_000);

  test('a destination that the stock firmware has open is E_FILE_IN_USE for a replace and a delete, and nothing changes', async () => {
    const dk = await rig.pausedBoot(await baseCard());
    await advance(rig.session, 2000);
    const before = (await snapshot('pdo0')).files;
    // the open-file bitmap (0x46f4ded0, one bit per inode id): every id below 2048 reads as open
    for (let i = 0; i < 64; i++) await pokeWord(rig.session, 0x46f4ded0 + i * 4, 0xffffffff);
    const r = await run(dk, WRITE | REPLACE | SAMPLES, 4000, 'Samples/old.bin');
    expect([r.create, r.job], 'the replace is refused before any data is written').toEqual([E_FILE_IN_USE, E_FILE_IN_USE]);
    const d = await run(dk, DELETE | SAMPLES_ONLY, 0, 'Samples/old.bin');
    expect(d.job, 'the delete is refused').toBe(E_FILE_IN_USE);
    for (let i = 0; i < 64; i++) await pokeWord(rig.session, 0x46f4ded0 + i * 4, 0);
    const s = await snapshot('pdo1');
    same(before, s.files, []);
    expect(staged(s.files)).toEqual([]);
    const ok = await run(dk, WRITE | REPLACE | SAMPLES, 4000, 'Samples/old.bin');
    expect([ok.close, ok.job], 'closed again: the replace works').toEqual([0, 0]);
  }, 1_800_000);

  // A cut at each step of the write (the adapter stops dead, as a power cut would; DKRT_PD_CUT). After the reboot the destination holds the old
  // file or the new one, whole, never nothing and never a partial file, the staging folder is cleaned, and no other file changed.
  const cuts: [string, number, boolean, boolean][] = [
    // [label, step, replace, new file at the destination afterwards]
    ['mid-data of a new file', 1, false, false],
    ['a new file hashed, before the rename', 2, false, false],
    ['a new file renamed', 5, false, true],
    ['mid-data of a replace', 1, true, false],
    ['a replace hashed, before the journal', 2, true, false],
    ['a replace with its journal written, before the delete', 3, true, false],
    ['a replace between the delete of the destination and the rename', 4, true, true],
    ['a replace renamed, before the journal is deleted', 5, true, true],
    ['a new file under two names (a cut inside the rename, no journal)', 6, false, true],
    ['a replace under two names (a cut inside the rename, with its journal)', 6, true, true],
  ];
  test('a power cut at any step leaves the old file or the new one, and the next boot cleans the staging folder', async () => {
    const data = pattern(50000);
    for (const [k, [label, step, replace, fresh]] of cuts.entries()) {
      const target = replace ? 'Samples/old.bin' : 'Samples/Cut/cut.bin';
      const dk = await rig.pausedBoot(await baseCard());
      await advance(rig.session, 2000);
      const before = (await snapshot(`pdc${k}a`)).files;
      await pokeKnob(rig.session, rig.plan, 'DKRT_PD_CUT', step);
      const r = await run(dk, WRITE | SAMPLES | (replace ? REPLACE : 0), data.length, target);
      expect(r.job, `${label}: the job ends with a write error (the cut)`).toBe(-57);
      const cut = (await snapshot(`pdc${k}b`)).card;
      // boot the card the cut left; the runtime's recovery runs in the UI loop
      const dk2 = await rig.pausedBoot(cut);
      await advance(rig.session, 3000);
      const s = await snapshot(`pdc${k}c`);
      const here = s.files.get(target);
      if (fresh) {
        expect(here, `${label}: the new file is at the destination`).toEqual({ size: data.length + 16, hash: slot4(data) });
        expect((await cat(s.card, target)).subarray(0, data.length)).toEqual(data);
      } else if (replace) {
        expect(here, `${label}: the old file is at the destination`).toEqual(before.get(target));
      } else {
        expect(here, `${label}: nothing at the destination (a new file that never completed)`).toBeUndefined();
      }
      expect(staged(s.files), `${label}: the staging folder is empty`).toEqual([]);
      expect(noStagingFolder(s.files), `${label}: and removed`).toBe(true);
      same(before, s.files, [target, 'Samples/Cut/']);
      // the data of the destination is still allocated: another file is written and the destination reads as before
      const more = pattern(40000);
      const m = await run(dk2, WRITE | SAMPLES, more.length, 'Samples/after.bin');
      expect([m.close, m.job], `${label}: the next write works`).toEqual([0, 0]);
      const t = await snapshot(`pdc${k}d`);
      expect(t.files.get(target), `${label}: the destination is unchanged by it`).toEqual(here);
      if (here && fresh) expect((await cat(t.card, target)).subarray(0, data.length), `${label}: its bytes`).toEqual(data);
      expect((await cat(t.card, 'Samples/after.bin')).subarray(0, more.length)).toEqual(more);
    }
  }, 3_600_000);

  test('a rename that fails after the destination was deleted is finished by the recovery without a reboot', async () => {
    const dk = await rig.pausedBoot(await baseCard());
    await advance(rig.session, 2000);
    const before = (await snapshot('pdr0')).files;
    const data = pattern(20000);
    await pokeKnob(rig.session, rig.plan, 'DKRT_PD_CUT', 7);
    const r = await run(dk, WRITE | REPLACE | SAMPLES, data.length, 'Samples/old.bin');
    expect(r.job, 'the caller is told: write error').toBe(-57);
    await advance(rig.session, 3000); // the UI pass runs the recovery again
    const s = await snapshot('pdr1');
    expect(s.files.get('Samples/old.bin'), 'the new file is at the destination').toEqual({ size: data.length + 16, hash: slot4(data) });
    expect((await cat(s.card, 'Samples/old.bin')).subarray(0, data.length)).toEqual(data);
    expect(staged(s.files)).toEqual([]);
    expect(noStagingFolder(s.files)).toBe(true);
    same(before, s.files, ['Samples/old.bin']);
    await pokeKnob(rig.session, rig.plan, 'DKRT_PD_CUT', 0);
    const again = await run(dk, WRITE | SAMPLES, 3000, 'Samples/next.bin');
    expect([again.close, again.job], 'the next file is accepted').toEqual([0, 0]);
    await rig.session.expectNoFault('rename failure');
  }, 1_800_000);

  test('an unhashed file at the destination (a stock writer that died) is a file that exists: E_EXISTS without REPLACE, replaced through the journal with it', async () => {
    const dk = await rig.pausedBoot(await baseCard());
    await advance(rig.session, 2000);
    await pokeKnob(rig.session, rig.plan, 'DKRT_PD_CUT', 8);
    const r = await run(dk, WRITE | SAMPLES, 7000, 'Samples/unhashed.bin');
    expect(r.job).toBe(-57);
    const cut = (await snapshot('pdu0')).card;
    const dk2 = await rig.pausedBoot(cut);
    await advance(rig.session, 3000);
    const before = (await snapshot('pdu1')).files;
    expect(before.has('Samples/unhashed.bin'), 'the file is on the card').toBe(true);
    const ex = await run(dk2, WRITE | SAMPLES, 777, 'Samples/unhashed.bin');
    expect([ex.create, ex.job], 'it exists').toEqual([E_EXISTS, E_EXISTS]);
    const data = pattern(6000);
    const rep = await run(dk2, WRITE | REPLACE | SAMPLES, data.length, 'Samples/unhashed.bin');
    expect([rep.close, rep.job], 'replaced').toEqual([0, 0]);
    const s = await snapshot('pdu2');
    expect(s.files.get('Samples/unhashed.bin')).toEqual({ size: data.length + 16, hash: slot4(data) });
    expect(staged(s.files)).toEqual([]);
    same(before, s.files, ['Samples/unhashed.bin']);
  }, 1_800_000);

  test('entries in the staging folder that are not ours are left alone and logged; the folder stays then', async () => {
    const foreign = pattern(100);
    const card = await baseCard({ [`${STAGING}/foreign.bin`]: foreign }, 'pd-foreign');
    const dk = await rig.pausedBoot(card);
    await advance(rig.session, 3000); // the recovery
    const before = (await snapshot('pdf0')).files;
    expect(before.has(`${STAGING}/foreign.bin`), 'recovery left it').toBe(true);
    const data = pattern(5000);
    const w = await run(dk, WRITE | REPLACE | SAMPLES, data.length, 'Samples/old.bin');
    expect([w.close, w.job]).toEqual([0, 0]);
    const s = await snapshot('pdf1');
    expect(staged(s.files), 'only the foreign file is in the folder').toEqual([`${STAGING}/foreign.bin`]);
    expect(s.files.get('Samples/old.bin')).toEqual({ size: data.length + 16, hash: slot4(data) });
    same(before, s.files, ['Samples/old.bin']);
    const next = await rig.pausedBoot(await rig.after('pdf2'));
    await advance(rig.session, 3000);
    void next;
    const t = await snapshot('pdf3');
    expect(staged(t.files), 'a reboot does not touch it').toEqual([`${STAGING}/foreign.bin`]);
  }, 1_800_000);
});
