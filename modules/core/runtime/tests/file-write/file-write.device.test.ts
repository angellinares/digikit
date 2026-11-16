import { be32, fileHash, type DkSession } from '@digi/dk-session';
import { beginBody, dataBody, GCMD } from '@digi/regression/runtime/upload-kit.ts';
import { describe, expect, test } from 'vitest';
import { bootInfo, pokeKnob, sha, statusOf } from '@digi/regression/runtime/card-kit.ts';
import type { TestRuntime } from '@digi/regression/support.ts';
import { cardRig, RT } from '@digi/regression/runtime/card-rig.ts';
import { SETTLE, word } from '@digi/regression/runtime/fx-live.ts';
import { planSymbol } from '@digi/regression/runtime/runtime-plan.ts';
import { DEVICES, FIRMWARE } from '@digi/regression/runtime/devices.ts';
import { RING_SYMBOLS } from '@digi/regression/runtime/runtime-menu.ts';

/**
 * Store file writes from a module (api 1.10: <dk/platform/file.h>, <dk/platform/io.h>, <dk/platform/hash32.h>). The fixture test/file-write starts an I/O job from a service
 * command: it writes a file of a known pattern, 4096 bytes a step, and reports what the job saw (tests/fixtures/modules/file-write).
 */

const CMD = 0x24; // START; +1 STATUS, +2 RELEASE, +3 CONTEXT. test/file-write-b answers the same commands at 0x28.
const CMD_B = 0x28;
const E_BUSY = -12;
const E_EXISTS = -18;
const E_CONTEXT = -19;
const E_CONTRACT = -48;
const E_HASH = -56;
const E_STATE = -53;
const WRITE = 1; // correct hash4
const WRITE_NO_HASH = 4;
const ABORT = 2;
const HOLD = 3;
const WRITE_BAD_HASH = 5;
const DELETE = 6;
const SPIN = 7;
const STALE = 8;
const FOREIGN = 9;
const BIG_CALL = 10;
const BATCH = 11; // the batch of deferred files (DK_FILE_DEFER): after the path, u8 n files and u8 mode
const COMMIT_THEN_END = 0; // mode: dk_files_commit after the last close
const END_JOB = 1; // mode: the job ends after the last close (the runtime commits at the end of the job)
const WAIT_IDLE = 2; // mode: the job sleeps after the last close (the runtime's 1 s idle commit names the files)
const REPLACE = 0x40; // added to the case: DK_FILE_REPLACE
const SELECTION = ['modules/file-write.dkm', 'modules/file-write-b.dkm'];
const pattern = (n: number) => Buffer.from(Uint8Array.from({ length: n }, (_, i) => (i * 7 + (i >> 8)) & 255));
/** File i of a batch job: the pattern shifted by 13 * i bytes. */
const shifted = (n: number, i: number) => Buffer.from(Uint8Array.from({ length: n }, (_, k) => (((k + 13 * i) * 7) + ((k + 13 * i) >> 8)) & 255));
const cstr = (s: string) => Buffer.concat([Buffer.from(s), Buffer.from([0])]);

for (const device of DEVICES) {
  describe(`${device.toUpperCase()} ${FIRMWARE[device]} store file writes from an I/O job`, () => {
    const kit = cardRig(device, 'file-write', {
      dsp: 'zero',
      roots: RT,
      symbols: ['DKRT_MAILBOX', 'DKRT_CARD', 'DKRT_SESSION_IDLE', 'DKRT_UPLOAD_CUT', 'DKRT_UPLOAD_WRITES', ...RING_SYMBOLS],
      initial: () => ({ selection: [] }),
      project: true,
      uploads: true,
      relaxFs: true,
    });
    const spec = { modules: ['test/file-write', 'test/file-write-b'], selection: SELECTION };
    const usb = <T>(rt: TestRuntime, run: (dk: DkSession) => Promise<T>) => kit.usb(rt, run);

    const start = async (rt: TestRuntime, kind: number, size: number, path: string, base = CMD) =>
      (await usb(rt, (dk) => dk.request(base, Buffer.concat([Buffer.from([kind]), be32(size), cstr(path)])))).status;
    const status = async (rt: TestRuntime, base = CMD) => {
      const r = await usb(rt, (dk) => dk.request(base + 1));
      const b = Buffer.from(r.body);
      return {
        state: b[0]!,
        create: b.readInt32BE(1),
        close: b.readInt32BE(5),
        abort: b.readInt32BE(9),
        job: b.readInt32BE(13),
        modHash: b.readUInt32BE(17),
        devHash: b.subarray(21, 25),
        steps: b.readUInt32BE(25),
        written: b.readUInt32BE(29),
        id: b.readInt32BE(33),
        aux: [b.readInt32BE(37), b.readInt32BE(41), b.readInt32BE(45)],
      };
    };
    /** Run the emulator until the job reaches `state` (1 running, 2 finished) and, when given, has written `written` bytes. */
    const until = async (rt: TestRuntime, state: number, written?: number, base = CMD) => {
      for (let i = 0; i < 100; i++) {
        await rt.ui.wait({ ms: 100, why: SETTLE });
        const s = await status(rt, base);
        if (s.state === state && (written === undefined || s.written === written)) return s;
      }
      throw new Error(`the job did not reach state ${state}`);
    };
    /** Emulated time in slices that each stay inside the step deadline of the harness. */
    const advanceLong = async (rt: TestRuntime, ms: number) => {
      for (let left = ms; left > 0; left -= 1000) await rt.ui.wait({ ms: Math.min(1000, left), why: SETTLE });
    };
    /** The store's files besides the module itself. */
    const files = async (rt: TestRuntime) =>
      new Map((await usb(rt, (dk) => dk.list())).filter((e) => !SELECTION.includes(e.path)).map((e) => [e.path, e]));
    const boot = async (tag: string) => {
      const rt = await kit.load(tag, spec);
      expect((await bootInfo(rt.session, kit.plan)).results, 'both modules load').toEqual([0, 0]);
      return rt;
    };

    test('a file written by the module is in the store with its size and hash; a second create of it is E_EXISTS', async () => {
      const rt = await boot('write');
      const size = 3 * 4096 + 777; // several steps and a tail that is not a multiple of 512
      const data = pattern(size);
      const before = (await rt.session.client.status()).heartbeat.stall_count;
      expect(await start(rt, WRITE, size, 'data/sdk-a.bin'), 'the job is queued').toBe(0);
      const s = await until(rt, 2);
      expect([s.create > 0, s.close, s.job, s.written], 'created, closed, finished').toEqual([true, 0, 0, size]);
      expect(s.steps, 'one step per chunk and more').toBeGreaterThanOrEqual(5);
      const slot = Buffer.from(fileHash(data));
      const f = (await files(rt)).get('data/sdk-a.bin');
      expect(f?.size, 'LIST has the file with its size').toBe(size);
      expect(f?.hash, 'and the file hash of the pattern').toBe(sha(data));
      expect(s.devHash, 'file_close wrote the same hash').toEqual(slot.subarray(0, 4));
      expect(s.modHash, 'hash32_* in the module gives the file hash').toBe(slot.readUInt32BE(0));
      expect(await usb(rt, (dk) => dk.verify('data/sdk-a.bin')), 'VERIFY reads the card and agrees').toBe(sha(data));
      expect(Buffer.from((await usb(rt, (dk) => dk.read('data/sdk-a.bin', 4000, 600))).bytes)).toEqual(data.subarray(4000, 4600));

      expect(await start(rt, WRITE, size, 'data/sdk-a.bin')).toBe(0);
      const again = await until(rt, 2);
      expect([again.create, again.job], 'no overwrite: file_create says E_EXISTS').toEqual([E_EXISTS, E_EXISTS]);
      expect((await files(rt)).get('data/sdk-a.bin')?.hash, 'the file is as it was').toBe(sha(data));

      expect(await start(rt, WRITE_NO_HASH, 700, 'data/sdk-b.bin')).toBe(0);
      expect((await until(rt, 2)).close, 'a file without an expected hash').toBe(0);
      expect((await files(rt)).get('data/sdk-b.bin')?.hash).toBe(sha(pattern(700)));
      await rt.session.expectNoFault('SDK writes');
      expect((await rt.session.client.status()).heartbeat.stall_count, 'the I/O task did not stall the UI').toBe(before);
    }, 900_000);

    test('a replace swaps the file atomically: the new bytes verify, an abort leaves the old file, and a path that is not there is created', async () => {
      const rt = await boot('replace');
      const old = pattern(3000);
      expect(await start(rt, WRITE, 3000, 'data/r.bin')).toBe(0);
      expect((await until(rt, 2)).close).toBe(0);

      // a replace that is aborted after one chunk leaves the old file whole
      expect(await start(rt, ABORT | REPLACE, 9000, 'data/r.bin')).toBe(0);
      const ab = await until(rt, 2);
      expect([ab.create > 0, ab.abort, ab.job], 'create with REPLACE passes, then file_abort').toEqual([true, 0, 0]);
      expect((await files(rt)).get('data/r.bin')?.hash, 'the old file is untouched').toBe(sha(old));
      expect(await usb(rt, (dk) => dk.verify('data/r.bin'))).toBe(sha(old));

      // a wrong expected hash leaves the old file too
      expect(await start(rt, WRITE_BAD_HASH | REPLACE, 9000, 'data/r.bin')).toBe(0);
      expect((await until(rt, 2)).close, 'E_HASH').toBe(E_HASH);
      expect((await files(rt)).get('data/r.bin')?.hash).toBe(sha(old));

      // the replace itself
      const fresh = pattern(9000);
      expect(await start(rt, WRITE | REPLACE, 9000, 'data/r.bin')).toBe(0);
      const done = await until(rt, 2);
      expect([done.create > 0, done.close, done.job, done.written]).toEqual([true, 0, 0, 9000]);
      const f = (await files(rt)).get('data/r.bin');
      expect([f?.size, f?.hash], 'the new size and hash').toEqual([9000, sha(fresh)]);
      expect(await usb(rt, (dk) => dk.verify('data/r.bin')), 'VERIFY reads the new bytes').toBe(sha(fresh));
      expect(Buffer.from((await usb(rt, (dk) => dk.read('data/r.bin', 4000, 600))).bytes)).toEqual(fresh.subarray(4000, 4600));

      // REPLACE on a path that is not there is a plain create; without it the existing file is still E_EXISTS
      expect(await start(rt, WRITE | REPLACE, 700, 'data/new.bin')).toBe(0);
      expect((await until(rt, 2)).close).toBe(0);
      expect((await files(rt)).get('data/new.bin')?.hash).toBe(sha(pattern(700)));
      expect(await start(rt, WRITE, 700, 'data/r.bin')).toBe(0);
      expect((await until(rt, 2)).create, 'no REPLACE: E_EXISTS').toBe(E_EXISTS);
      await rt.session.expectNoFault('replace');
    }, 900_000);

    test('the samples root: the Digitone II store folder samples/, and on the Digitakt II E_CONTRACT while core/plusdrive-write is not in the image', async () => {
      const rt = await boot('root');
      expect(await start(rt, WRITE | 0x80, 2000, 'a.bin')).toBe(0);
      const s = await until(rt, 2);
      if (device === 'dn2') {
        expect([s.create > 0, s.close], 'samples/ in the store').toEqual([true, 0]);
        expect((await files(rt)).get('samples/a.bin')?.hash).toBe(sha(pattern(2000)));
      } else {
        expect([s.create, s.job], 'no adapter').toEqual([E_CONTRACT, E_CONTRACT]);
      }
    }, 900_000);

    test('a wrong expected hash is E_HASH and leaves nothing; an abort leaves nothing and frees the session; a delete removes the file', async () => {
      const rt = await boot('abort');
      expect(await start(rt, WRITE_BAD_HASH, 9000, 'data/bad.bin')).toBe(0);
      const bad = await until(rt, 2);
      expect([bad.close, bad.job], 'file_close checks hash4').toEqual([E_HASH, E_HASH]);
      expect([...(await files(rt)).keys()], 'nothing was stored').toEqual([]);

      expect(await start(rt, ABORT, 9000, 'data/aborted.bin')).toBe(0);
      const ab = await until(rt, 2);
      expect([ab.abort, ab.job, ab.written], 'one chunk written, then file_abort').toEqual([0, 0, 4096]);
      expect([...(await files(rt)).keys()], 'nothing was stored').toEqual([]);

      expect(await start(rt, WRITE, 5000, 'data/aborted.bin'), 'the same path can be written now').toBe(0);
      expect((await until(rt, 2)).close).toBe(0);
      expect((await files(rt)).get('data/aborted.bin')?.hash).toBe(sha(pattern(5000)));

      expect(await start(rt, DELETE, 0, 'data/aborted.bin')).toBe(0);
      expect((await until(rt, 2)).close, 'file_delete').toBe(0);
      expect([...(await files(rt)).keys()], 'the file is gone').toEqual([]);
      await rt.session.expectNoFault('abort and delete');
    }, 900_000);

    test('a SysEx write command while the module has a session open is E_BUSY; the file calls outside an I/O step are E_CONTEXT', async () => {
      const rt = await boot('busy');
      const ctx = await usb(rt, (dk) => dk.request(CMD + 3));
      expect([ctx.status, Buffer.from(ctx.body).readInt32BE(0)], 'file_create in the MIDI task').toEqual([0, E_CONTEXT]);

      expect(await start(rt, HOLD, 20000, 'data/held.bin')).toBe(0);
      // the retries below take emulated time: the idle expiry (20 s) must not end the session under test
      await pokeKnob(rt.session, kit.plan, 'DKRT_SESSION_IDLE', 0x7fffffff);
      const held = await until(rt, 1, 4096); // one chunk written, the job sleeps with the session open
      expect(held.create).toBeGreaterThan(0);
      const upload = Buffer.from('sysex');
      expect(await statusOf(() => usb(rt, (dk) => dk.put('data/sysex.bin', upload))), 'SysEx BEGIN while the SDK session is open').toBe(E_BUSY);
      expect(await statusOf(() => usb(rt, (dk) => dk.mkdir('data'))), 'MKDIR does not abort it').toBe(E_BUSY);
      expect(await statusOf(() => usb(rt, (dk) => dk.delete('data/none.bin'))), 'DELETE neither').toBe(E_BUSY);
      expect((await status(rt)).state, 'the job still holds its session').toBe(1);
      expect(await statusOf(() => usb(rt, (dk) => dk.list().then(() => undefined))), 'LIST reads the table as ever').toBe(0);
      expect(await statusOf(() => usb(rt, (dk) => dk.info().then(() => undefined)))).toBe(0);

      expect((await usb(rt, (dk) => dk.request(CMD + 2))).status, 'io_wake').toBe(0);
      const done = await until(rt, 2);
      expect([done.abort, done.job], 'the woken job aborted its file and ended').toEqual([0, 0]);
      expect([...(await files(rt)).keys()], 'the held file never reached the table').toEqual([]);
      await usb(rt, (dk) => dk.put('data/sysex.bin', upload));
      expect((await files(rt)).get('data/sysex.bin')?.hash, 'SysEx writes again').toBe(sha(upload));
      await rt.session.expectNoFault('E_BUSY');
    }, 900_000);

    test('file_* is E_CONTEXT in the MIDI task even while a step is running; a stale or another module\'s handle is E_STATE', async () => {
      const rt = await boot('context');
      expect(await start(rt, SPIN, 4096, 'data/spin.bin')).toBe(0);
      let s = await status(rt);
      for (let i = 0; i < 50 && s.create <= 0; i++) {
        await rt.ui.wait({ ms: 100, why: SETTLE });
        s = await status(rt);
      }
      expect(s.create, 'the step made its file and spins with the session open').toBeGreaterThan(0);
      const handle = s.create;
      const ctx = Buffer.from((await usb(rt, (dk) => dk.request(CMD + 3, be32(handle)))).body);
      expect(ctx[4], 'the job is mid-step (its step is spinning)').toBe(1);
      expect([ctx.readInt32BE(0), ctx.readInt32BE(5), ctx.readInt32BE(9)], 'file_create, file_write and file_abort in the MIDI task').toEqual([
        E_CONTEXT,
        E_CONTEXT,
        E_CONTEXT,
      ]);
      expect((await status(rt)).state, 'the spinning job still has its session').toBe(1);

      expect((await usb(rt, (dk) => dk.request(CMD + 2))).status).toBe(0); // end the spin (one I/O task: a spinning step would hold the other module's job)
      const done = await until(rt, 2);
      expect([done.abort, done.job]).toEqual([0, 0]);

      // a job of the other module that uses the handle of a session of the first (the first job sleeps with its session open)
      expect(await start(rt, HOLD, 20000, 'data/held.bin')).toBe(0);
      const held = await until(rt, 1, 4096);
      expect(await start(rt, FOREIGN, held.create, 'x', CMD_B)).toBe(0);
      const other = await until(rt, 2, undefined, CMD_B);
      expect(other.aux, "another module's handle: file_write, file_close and file_abort say E_STATE").toEqual([E_STATE, E_STATE, E_STATE]);
      expect((await status(rt)).state, "and the first module's session is untouched").toBe(1);
      expect((await usb(rt, (dk) => dk.request(CMD + 2))).status).toBe(0);
      expect((await until(rt, 2)).abort).toBe(0);

      // a handle that was aborted is stale
      expect(await start(rt, STALE, 4096, 'data/stale.bin')).toBe(0);
      const stale = await until(rt, 2);
      expect(stale.aux, 'file_write, file_close and file_abort on a closed handle say E_STATE').toEqual([E_STATE, E_STATE, E_STATE]);
      expect([...(await files(rt)).keys()], 'nothing was stored').toEqual([]);
      await rt.session.expectNoFault('E_CONTEXT');
    }, 900_000);

    test('one file_write call of 64 KiB and more completes', async () => {
      const rt = await boot('big');
      const size = 65536 + 700;
      const data = pattern(size);
      expect(await start(rt, BIG_CALL, size, 'data/big.bin')).toBe(0);
      const s = await until(rt, 2);
      expect([s.close, s.job, s.written], 'two calls: 65536 bytes (two 32 KiB writes inside) and the tail').toEqual([0, 0, size]);
      expect(s.steps, 'create, the 64 KiB call, the tail call, close').toBeLessThanOrEqual(6);
      expect((await files(rt)).get('data/big.bin')?.hash).toBe(sha(data));
      expect(await usb(rt, (dk) => dk.verify('data/big.bin'))).toBe(sha(data));
    }, 900_000);

    test('an abandoned SysEx BEGIN is aborted after 20 s and the SDK can write again', async () => {
      const rt = await boot('idle');
      const bytes = Buffer.alloc(2048, 7);
      const begin = (path: string) => usb(rt, (dk) => dk.request(GCMD.BEGIN, beginBody(path, bytes.length, fileHash(bytes))));
      expect((await begin('data/abandoned.bin')).status, 'BEGIN, and then the host goes away').toBe(0);
      expect(await start(rt, WRITE, 1000, 'data/while-open.bin'), 'a job starts').toBe(0);
      const blocked = await until(rt, 2);
      expect(blocked.create, 'the SDK cannot create a file while a SysEx session is open').toBe(E_BUSY);
      await advanceLong(rt, 19_000);
      expect((await usb(rt, (dk) => dk.request(GCMD.DATA, dataBody(0, bytes)))).status, 'a write at 19 s: the session is there and its 20 s start again').toBe(0);
      await advanceLong(rt, 21_000);
      expect((await usb(rt, (dk) => dk.request(GCMD.DATA, dataBody(2048, bytes.subarray(0, 512))))).status, '21 s without a write: no upload is open').toBe(E_STATE);
      expect(await start(rt, WRITE, 1000, 'data/after-expiry.bin')).toBe(0);
      expect((await until(rt, 2)).close, 'the SDK writes again').toBe(0);
      expect((await files(rt)).get('data/after-expiry.bin')?.hash).toBe(sha(pattern(1000)));
      expect([...(await files(rt)).keys()], 'the abandoned file never reached the store').toEqual(['data/after-expiry.bin']);
    }, 900_000);
    /** A batch job: `n` deferred files `<base>00`, `<base>01`, ... of `size` bytes each. */
    const startBatch = async (rt: TestRuntime, base: string, n: number, size: number, mode: number) =>
      (await usb(rt, (dk) => dk.request(CMD, Buffer.concat([Buffer.from([BATCH]), be32(size), cstr(base), Buffer.from([n, mode])])))).status;
    /** Run until the batch job has closed `n` files (it may still be waiting). */
    const closed = async (rt: TestRuntime, n: number) => {
      for (let i = 0; i < 200; i++) {
        await rt.ui.wait({ ms: 50, why: SETTLE });
        const s = await status(rt);
        if (s.steps >= n || s.state === 2) return s;
      }
      throw new Error(`the job did not close ${n} files`);
    };
    const names = (m: Map<string, unknown>) => [...m.keys()].sort();
    const batchNames = (base: string, from: number, to: number) =>
      Array.from({ length: to - from }, (_, k) => `${base}${String(from + k).padStart(2, '0')}`);

    test('deferred files: pending until dk_files_commit, then all named by one commit with their own hashes', async () => {
      const rt = await boot('defer-commit');
      const size = 2000;
      expect(await startBatch(rt, 'batch/c', 5, size, COMMIT_THEN_END), 'the job is queued').toBe(0);
      const s = await until(rt, 2);
      expect([s.job, s.close, s.steps], 'five files closed, the job ended ok').toEqual([0, 0, 5]);
      expect(s.aux, 'five pending after the last close, the commit returned 0, none pending after it').toEqual([5, 0, 0]);
      const got = await files(rt);
      expect(names(got), 'all five are in the store').toEqual(batchNames('batch/c', 0, 5));
      for (let i = 0; i < 5; i++) {
        expect(got.get(`batch/c${String(i).padStart(2, '0')}`)?.hash, `file ${i} has its own hash`).toBe(sha(shifted(size, i)));
      }
      expect(await usb(rt, (dk) => dk.verify('batch/c03')), 'VERIFY reads the card and agrees').toBe(sha(shifted(size, 3)));
      await rt.session.expectNoFault('a batch commit');
    }, 900_000);

    test('the limit of 16 pending files commits them all inside the closing call; the 17th waits for the idle commit', async () => {
      const rt = await boot('defer-limit');
      expect(await startBatch(rt, 'batch/l', 17, 1000, WAIT_IDLE)).toBe(0);
      const s = await closed(rt, 17);
      expect(s.state, 'the job sleeps after the 17th close').toBe(1);
      expect(s.aux[0], 'one pending: the 16th close committed the first 16').toBe(1);
      expect(names(await files(rt)), 'the first 16 are in the store, the 17th is not').toEqual(batchNames('batch/l', 0, 16));
      await advanceLong(rt, 2000);
      expect(names(await files(rt)), 'after the idle second the 17th is named too').toEqual(batchNames('batch/l', 0, 17));
      await usb(rt, (dk) => dk.request(CMD + 2));
      const end = await until(rt, 2);
      expect([end.job, end.aux[2]], 'the job ends ok with nothing pending').toEqual([0, 0]);
    }, 900_000);

    test('the idle commit: pending files are not in the store until 1 s after the last file call; a delete of a pending path removes it', async () => {
      const rt = await boot('defer-idle');
      expect(await startBatch(rt, 'batch/i', 3, 1500, WAIT_IDLE)).toBe(0);
      const s = await closed(rt, 3);
      expect([s.state, s.aux[0]], 'three pending, the job sleeps').toEqual([1, 3]);
      expect(names(await files(rt)), 'a pending file is not listed').toEqual([]);
      await usb(rt, (dk) => dk.delete('batch/i00'));
      await advanceLong(rt, 2000);
      expect(names(await files(rt)), 'the deleted pending file never reaches the store; the others are named by the idle commit').toEqual(batchNames('batch/i', 1, 3));
      expect((await files(rt)).get('batch/i01')?.hash).toBe(sha(shifted(1500, 1)));
      await usb(rt, (dk) => dk.request(CMD + 2));
      expect((await until(rt, 2)).job).toBe(0);
    }, 900_000);

    test('the end of the owner\'s job commits its pending files without waiting a second', async () => {
      const rt = await boot('defer-end');
      expect(await startBatch(rt, 'batch/e', 4, 1200, END_JOB)).toBe(0);
      const s = await until(rt, 2);
      expect([s.job, s.aux[0]], 'four pending when the job ended').toEqual([0, 4]);
      await rt.ui.wait({ ms: 300, why: SETTLE });
      expect(names(await files(rt)), 'named well before the 1 s idle time').toEqual(batchNames('batch/e', 0, 4));
    }, 900_000);
    test('a SysEx BEGIN of a pending path takes its slot (the pending bytes are never committed over it) and a path inside a pending file is E_PATH', async () => {
      const rt = await boot('defer-begin');
      expect(await startBatch(rt, 'batch/p', 2, 1500, WAIT_IDLE)).toBe(0);
      expect((await closed(rt, 2)).aux[0]).toBe(2);
      const E_PATH = -51;
      const inside = await usb(rt, (dk) => dk.request(GCMD.BEGIN, beginBody('batch/p00/x', 512, fileHash(Buffer.alloc(512)))));
      expect(inside.status, 'inside a pending file').toBe(E_PATH);
      const bytes = Buffer.alloc(2048, 9);
      await usb(rt, (dk) => dk.put('batch/p00', bytes)); // SysEx BEGIN, DATA, END of the pending path
      expect((await files(rt)).get('batch/p00')?.hash, 'the SysEx file is stored at once').toBe(sha(bytes));
      await advanceLong(rt, 2000);
      const got = await files(rt);
      expect(names(got), 'the other pending file is named by the idle commit').toEqual(['batch/p00', 'batch/p01']);
      expect(got.get('batch/p00')?.hash, 'the replaced pending entry did not come back').toBe(sha(bytes));
      expect(got.get('batch/p01')?.hash).toBe(sha(shifted(1500, 1)));
      await usb(rt, (dk) => dk.request(CMD + 2));
      expect((await until(rt, 2)).job).toBe(0);
    }, 900_000);

    test('a CLEAR whose commit is cut leaves the old store, and the pending files are still pending and committed afterwards', async () => {
      const rt = await boot('defer-clear');
      expect(await startBatch(rt, 'batch/old', 1, 1200, END_JOB)).toBe(0);
      await until(rt, 2);
      await rt.ui.wait({ ms: 300, why: SETTLE });
      expect(names(await files(rt))).toEqual(['batch/old00']);
      expect(await startBatch(rt, 'batch/p', 2, 1500, WAIT_IDLE)).toBe(0);
      expect((await closed(rt, 2)).aux[0], 'two pending').toBe(2);
      // refuse every card write from now on: the CLEAR commit fails
      const writes = await word(rt.session, planSymbol(kit.plan, 'DKRT_UPLOAD_WRITES'));
      await pokeKnob(rt.session, kit.plan, 'DKRT_UPLOAD_CUT', writes);
      await expect(usb(rt, (dk) => dk.clear()), 'the CLEAR is refused its write').rejects.toBeDefined();
      await pokeKnob(rt.session, kit.plan, 'DKRT_UPLOAD_CUT', 0);
      expect(names(await files(rt)), 'the old store is intact and the pending files are not listed').toEqual(['batch/old00']);
      await advanceLong(rt, 2500);
      expect(names(await files(rt)), 'the pending files survived the failed CLEAR and are committed').toEqual(['batch/old00', 'batch/p00', 'batch/p01']);
      await usb(rt, (dk) => dk.request(CMD + 2));
      expect((await until(rt, 2)).job).toBe(0);
    }, 900_000);
  });
}
