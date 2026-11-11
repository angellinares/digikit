import { execFile } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { promisify } from 'node:util';
import { be32, fileHash, type DkSession } from '@digi/dk-session';
import { repoRoot } from '@digi/test-support';
import { expect, test } from 'vitest';
import { overlayArgs, plusdriveBinary, pokeKnob, pokeWord } from '@digi/regression/runtime/card-kit.ts';
import { cardRigs, RT, type CardRig } from '@digi/regression/runtime/card-rig.ts';
import { advance } from '@digi/regression/runtime/fx-live.ts';

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
