import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { chooseBulk, EmuBulkLink, fileHash, type DkSession } from '@digi/dk-session';
import { repoRoot } from '@digi/test-support';
import { describe, expect, test } from 'vitest';
import { overlayArgs, plusdriveBinary } from '@digi/regression/runtime/card-kit.ts';
import { cardRig, RT } from '@digi/regression/runtime/card-rig.ts';
import { DEVICES, FIRMWARE } from '@digi/regression/runtime/devices.ts';
import { planSymbol } from '@digi/regression/runtime/runtime-plan.ts';
import { word, SETTLE } from '@digi/regression/runtime/fx-live.ts';

/**
 * core/sdhc-large through the real writers, driven over the usb-fast bulk link (the emulator's usb.bulk.in / usb.bulk.out).
 * DN2: the store writer registers its data buffer, so one 1022-block piece of an upload is one firmware block-write call (one card command); with the
 * switch off (usb-fast STATUS 'S' 3 0) the same piece is the firmware's 64 KiB split (nine calls: the call and its eight pieces). DT2: the +Drive writes go through the
 * firmware's own file code (no registered buffer: all stock) and `hash_fast` reads one 32 KiB page per call: nothing is raised.
 * DN2: the close reads the file back in 256 KiB pieces from a registered buffer (raised reads) with the switch on, 64 KiB without.
 * The adapter counts the calls of the firmware's block write and read (sdhc_large_ctl key 1; here read from its RAM). Data read back from the card equals what was sent.
 */
const exec = promisify(execFile);
const SAMPLES = 1;
const STATUS = 0x37;
const PIECE = 1022 * 512; // the largest card command: 1022 blocks
const pattern = (n: number, seed = 0) => Buffer.from(Uint8Array.from({ length: n }, (_, i) => (i * 7 + (i >> 8) + seed) & 255));
const withTail = (b: Buffer) => Buffer.concat([b, Buffer.alloc(16)]);
const hashU32 = (b: Buffer, dt2: boolean) => Buffer.from(fileHash(dt2 ? withTail(b) : b)).readUInt32BE(0);

for (const device of DEVICES) {
  const dt2 = device === 'dt2';
  describe(`${device.toUpperCase()} ${FIRMWARE[device]} core/sdhc-large`, () => {
    const rig = cardRig(device, 'sdhc-large', {
      dsp: 'zero',
      roots: [...RT, 'core/usb-bulk', 'core/sdhc-large', ...(dt2 ? ['core/plusdrive-write'] : [])],
      symbols: ['DKRT_MAILBOX', 'DKRT_SESSION_IDLE', 'sl_cnt', ...(dt2 ? ['DKRT_PD_CUT'] : [])],
      project: true,
      uploads: true,
      relaxFs: true,
    });
    const call = <T>(method: string, params?: Record<string, unknown>) => rig.session.client.call(method, params) as Promise<T>;
    /** [block write calls, of them raised, block read calls, of them raised] */
    const counters = async () => {
      const at = planSymbol(rig.plan, 'sl_cnt');
      return [await word(rig.session, at), await word(rig.session, at + 4), await word(rig.session, at + 8), await word(rig.session, at + 12)];
    };
    const delta = (a: number[], b: number[]) => b.map((v, i) => (v - a[i]!) >>> 0);
    /** STATUS 'S' [3 val]: the switch of core/sdhc-large through usb-fast (bit 3 of the mask; no val only reads). Returns the switch now. */
    const sw = async (dk: DkSession, val?: number) =>
      (Buffer.from((await dk.request(STATUS, Uint8Array.from(val === undefined ? [0x53] : [0x53, 3, val]))).body).readUInt32BE(0) >>> 3) & 1;
    const boot = async () => {
      const mod = rig.mod('digikit/usb-fast');
      const dk = await rig.pausedBoot(await rig.card({ name: 'sl-base', modules: [mod], selection: [mod.name] }));
      if (dt2) await rig.runtime.ui.wait({ ms: 2000, why: SETTLE }); // the boot recovery of the staging folder
      for (let i = 0; i < 100 && (await EmuBulkLink.connectCallback(call)) === undefined; i++) await rig.runtime.ui.wait({ ms: 100, why: SETTLE });
      const notes: string[] = [];
      const choice = await chooseBulk(dk, EmuBulkLink.opener(call, { stepInstructions: 100_000 }), (l) => notes.push(l), { timeoutMs: 20_000, resyncMs: 200, log: (l) => notes.push(l) });
      expect(choice, `a bulk link is negotiated: ${notes.join('; ')}`).toBeDefined();
      return { dk, bulk: choice! };
    };
    const catDt2 = async (name: string) => {
      const card = await rig.after(`sl-${name}`);
      const r = await exec(plusdriveBinary(), ['fs', 'cat', card, `Samples/${name}`, ...overlayArgs(card)], { cwd: repoRoot(), encoding: 'buffer', maxBuffer: 1 << 28 });
      return r.stdout.subarray(0, -16);
    };
    const expectOnCard = async (dk: DkSession, name: string, data: Buffer) => {
      if (dt2) expect(await catDt2(name), `${name}: bytes on the card`).toEqual(data);
      else {
        expect(await dk.verify(`samples/${name}`), `${name}: VERIFY reads the card`).toBe(Buffer.from(fileHash(data)).toString('hex'));
        const mid = Math.floor(data.length / 2);
        expect(Buffer.from((await dk.read(`samples/${name}`, mid, 600)).bytes), `${name}: bytes`).toEqual(data.subarray(mid, mid + 600));
      }
    };

    test(
      dt2
        ? 'the +Drive writes and the hash_fast reads stay stock'
        : 'a registered 1022-block piece of an upload is one card command and the readback is raised; the switch off gives the 64 KiB split and 64 KiB reads, and the data is the same',
      async () => {
        const { dk, bulk } = await boot();
        expect(await sw(dk), 'the switch is on at boot').toBe(1);
        const rel = (n: string) => (dt2 ? `Samples/${n}` : n);
        const put = async (name: string, data: Buffer) => {
          const before = await counters();
          expect(await bulk.session.put(SAMPLES, rel(name), data, { expected: hashU32(data, dt2) }), `${name}: END hash`).toBe(hashU32(data, dt2));
          return delta(before, await counters());
        };
        // two whole pieces: the staging buffer of usb-fast is the file, flushed by one file_write that the writer cuts in 1022-block pieces
        const a = pattern(2 * PIECE, 1);
        const b = pattern(2 * PIECE, 2);
        const on = await put('on.bin', a);
        expect(await sw(dk, 0), 'off now').toBe(0);
        const off = await put('off.bin', b);
        expect(await sw(dk, 1), 'back on').toBe(1);
        const again = await put('again.bin', pattern(2 * PIECE, 3));
        await expectOnCard(dk, 'on.bin', a);
        await expectOnCard(dk, 'off.bin', b);

        if (dt2) {
          expect([on[1], off[1], again[1]], 'no block write of the +Drive path is raised').toEqual([0, 0, 0]);
          // hash_fast reads one 32 KiB page per call: no read is raised
          expect([on[3], off[3], again[3]], 'no read exceeds 32 KiB').toEqual([0, 0, 0]);
        } else {
          expect(on[1], 'switch on: the two data pieces are the raised block writes').toBe(2);
          expect(off[1], 'switch off: none').toBe(0);
          expect(again[1]).toBe(2);
          // a piece of 1022 blocks is one call with the adapter and 1 + 8 calls without it (the firmware cuts it in 64 KiB pieces); the other writes are the same
          expect(off[0]! - on[0]!, 'two pieces: 8 more calls each').toBe(16);
          expect(again[0]).toBe(on[0]);
          // the close reads the file back in 256 KiB pieces with the switch on (raised reads) and in 64 KiB pieces with it off
          expect(on[3], 'switch on: the readback pieces are raised reads').toBeGreaterThan(0);
          expect(off[3], 'switch off: 64 KiB reads').toBe(0);
          expect(again[3]).toBeGreaterThan(0);
        }
        await rig.session.expectNoFault('sdhc-large');
      },
      3_600_000,
    );
  });
}
