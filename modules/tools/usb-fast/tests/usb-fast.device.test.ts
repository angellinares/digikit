import { execFile } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { promisify } from 'node:util';
import { BULK_CMD, BULK_STATUS } from '@digi/dk-session/gen_bulk';
import {
  BulkDeviceError,
  BulkSession,
  EmuBulkLink,
  EmuOutOnlyLink,
  bulkRequest,
  chooseBulk,
  fileHash,
  parseBulkReply,
  type BulkChoice,
  type BulkLink,
  type DkSession,
} from '@digi/dk-session';
import { repoRoot } from '@digi/test-support';
import { describe, expect, test } from 'vitest';
import { overlayArgs, plusdriveBinary, pokeKnob } from '@digi/regression/runtime/card-kit.ts';
import { cardRig, RT, type CardRig } from '@digi/regression/runtime/card-rig.ts';
import { portFile } from '@digi/regression/runtime/device-data.ts';
import { DEVICES, FIRMWARE } from '@digi/regression/runtime/devices.ts';
import { planSymbol } from '@digi/regression/runtime/runtime-plan.ts';
import { rpc, word, SETTLE } from '@digi/regression/runtime/fx-live.ts';

/**
 * digikit/usb-fast (docs/plans/usb-fast-hardware.md, step 4), through the TypeScript BulkSession over the emulator link (`EmuBulkLink`: the daemon's
 * usb.bulk.in / usb.bulk.out): uploads to the DN2 store and the DT2 +Drive with their hash checks, REPLACE and E_EXISTS, ABORT, a USB reset in the middle of a
 * file, frames that span receive buffers and several frames in one buffer (with and without the zero-length packet), the nonce handshake, power cuts, and
 * a bench line. The emulator does not model the USB controller: what it cannot show (enumeration, the real dTD engine, real throughput) is for the hardware run.
 */

const exec = promisify(execFile);
const SAMPLES = 1; // DK_FILES_SAMPLES: the store's samples/ folder on the DN2, the +Drive on the DT2
const STORE = 2; // DK_FILES_STORE
const E_EXISTS = BULK_STATUS.L_E_EXISTS;
const STAGING = '.dkstage';
const STEP = 100_000; // instructions an idle poll of the paused emulator steps (0.75 ms)
const pattern = (n: number, seed = 0) => Buffer.from(Uint8Array.from({ length: n }, (_, i) => (i * 7 + (i >> 8) + seed) & 255));
const withTail = (b: Buffer) => Buffer.concat([b, Buffer.alloc(16)]);
const slot = (b: Buffer, dt2: boolean) => Buffer.from(fileHash(dt2 ? withTail(b) : b));
/** The hash|1 as the u32 that END returns. */
const hashU32 = (b: Buffer, dt2: boolean) => slot(b, dt2).readUInt32BE(0);
const hex = (b: Buffer, dt2: boolean) => slot(b, dt2).toString('hex');

/**
 * The two routes of the core upload tests: the bulk pair of USB MIDI (port entry 2, replies on EP2 IN) and the OUT-only interface 5 of USB AUDIO/MIDI (port entry 5 with
 * core/usb-bulk-audio, replies over the SysEx 'R' poll). A mode picks the active port after the boot and the link class that talks to the guest.
 */
interface Mode {
  label: string;
  port: number;
  /** INFO flags bit 0: replies over SysEx. */
  outOnly: boolean;
  callback: typeof EmuBulkLink.connectCallback;
  opener: typeof EmuBulkLink.opener;
}
const PAIR: Mode = { label: 'bulk pair', port: 2, outOnly: false, callback: EmuBulkLink.connectCallback, opener: EmuBulkLink.opener };
const MODES: Mode[] = [
  PAIR,
  { label: 'OUT-only', port: 5, outOnly: true, callback: EmuOutOnlyLink.connectCallback, opener: EmuOutOnlyLink.opener },
];

interface Entry {
  size: number;
  hash: string;
}

async function drive(card: string): Promise<Map<string, Entry>> {
  const { stdout } = await exec(plusdriveBinary(), ['fs', 'ls', card, ...overlayArgs(card)], { cwd: repoRoot(), maxBuffer: 1 << 26 });
  const out = new Map<string, Entry>();
  for (const line of stdout.trim().split('\n')) {
    const [p, size, h] = line.split('\t') as [string, string, string];
    if (!p.endsWith('/')) out.set(p, { size: Number(size), hash: h });
  }
  return out;
}

async function cat(card: string, p: string): Promise<Buffer> {
  const r = await exec(plusdriveBinary(), ['fs', 'cat', card, p, ...overlayArgs(card)], { cwd: repoRoot(), encoding: 'buffer', maxBuffer: 1 << 28 });
  return r.stdout;
}

for (const device of DEVICES) {
  const dt2 = device === 'dt2';
  describe(`${device.toUpperCase()} ${FIRMWARE[device]} digikit/usb-fast`, () => {
    const rig: CardRig = cardRig(device, 'usb-fast', {
      dsp: 'zero',
      roots: [...RT, 'core/usb-bulk', 'core/usb-bulk-audio', 'core/sdhc-large', ...(dt2 ? ['core/plusdrive-write'] : [])],
      symbols: ['DKRT_MAILBOX', 'DKRT_SESSION_IDLE', 'DKRT_UPLOAD_CUT', 'DKRT_UPLOAD_TEAR', 'DKRT_UPLOAD_WRITES', 'DKRT_UPLOAD_FLIP', 'usb_bulk_restarts', ...(dt2 ? ['DKRT_PD_CUT'] : [])],
      project: true,
      uploads: true,
      relaxFs: true,
    });
    /** The path under the samples root: the +Drive's Samples folder is part of the path on the DT2, the store's samples/ folder is the root itself on the DN2. */
    const rp = (name: string) => (dt2 ? `Samples/${name}` : name);
    let connects = 0;
    const recovered = new WeakSet<object>();
    const call = <T>(method: string, params?: Record<string, unknown>) => rig.session.client.call(method, params) as Promise<T>;
    const SEEDS: Record<string, Buffer> = { 'old.bin': pattern(5000, 3), 'keep.bin': pattern(1300, 5) };

    /** A card with usb-fast selected and two sample files; on the DT2 they are in the +Drive's Samples folder, on the DN2 in the store's samples/ folder. */
    const bases = new Map<string, string>();
    const baseCard = async (name = 'uf-base') => {
      const known = bases.get(name);
      if (known) return known;
      const mod = rig.mod('digikit/usb-fast');
      const files = Object.entries(SEEDS).map(([n, bytes]) => {
        const file = path.join(rig.artifacts, `seed-${n}`);
        fs.writeFileSync(file, bytes);
        return { name: dt2 ? `Samples/${n}` : `samples/${n}`, file };
      });
      const card = await rig.card(
        dt2 ? { name, modules: [mod], drive: files, selection: [mod.name] } : { name, modules: [mod, ...files], selection: [mod.name] },
      );
      bases.set(name, card);
      return card;
    };

    /** The files of the destination folder as the device holds them now (name, size and hash), from the card after the guest's writes. */
    const state = async (dk: DkSession, tag: string): Promise<{ files: Map<string, Entry>; card?: string }> => {
      if (dt2) {
        const card = await rig.after(tag);
        const all = await drive(card);
        const files = new Map<string, Entry>();
        for (const [p, e] of all) if (p.startsWith('Samples/')) files.set(p.slice('Samples/'.length), e);
        for (const p of all.keys()) expect(p.startsWith(`${STAGING}/`), `no staging file left: ${p}`).toBe(false);
        return { files, card };
      }
      const files = new Map<string, Entry>();
      for (const e of await dk.list()) if (e.path.startsWith('samples/')) files.set(e.path.slice('samples/'.length), { size: e.size, hash: e.hash });
      return { files };
    };
    /** The file is there with these bytes: size, hash, and the bytes themselves from the card (DT2) or by a read and VERIFY over SysEx (DN2). */
    const expectFile = async (dk: DkSession, snap: Awaited<ReturnType<typeof state>>, name: string, data: Buffer, what: string) => {
      const e = snap.files.get(name);
      expect(e, `${what}: ${name} is there`).toBeDefined();
      expect(e!.size, `${what}: size`).toBe(dt2 ? data.length + 16 : data.length);
      expect(e!.hash, `${what}: hash`).toBe(dt2 ? hex(data, true).slice(0, 8) : hex(data, false));
      if (dt2) expect((await cat(snap.card!, `Samples/${name}`)).subarray(0, data.length), `${what}: bytes`).toEqual(data);
      else {
        expect(await dk.verify(`samples/${name}`), `${what}: VERIFY reads the card`).toBe(hex(data, false));
        const mid = Math.floor(data.length / 2);
        expect(Buffer.from((await dk.read(`samples/${name}`, mid, Math.min(600, data.length - mid))).bytes), `${what}: bytes`).toEqual(data.subarray(mid, mid + Math.min(600, data.length - mid)));
      }
    };
    const unchanged = (before: Map<string, Entry>, after: Map<string, Entry>, except: string[]) => {
      const strip = (m: Map<string, Entry>) => [...m].filter(([p]) => !except.includes(p)).sort(([a], [b]) => a.localeCompare(b));
      expect(strip(after), 'no other file changed').toEqual(strip(before));
    };

    /** Run until the module's job has enabled the interface (the guest's port table points at the adapter), then do what the host does: SET_NONCE over SysEx, find the device by its nonce. */
    const connect = async (
      dk: DkSession,
      options: { stepInstructions?: number; zlp?: boolean; timeoutMs?: number; chunk?: number; mode?: Mode } = {},
    ): Promise<BulkChoice> => {
      const mode = options.mode ?? PAIR;
      if (dt2 && !recovered.has(dk)) {
        recovered.add(dk);
        await rig.runtime.ui.wait({ ms: 2000, why: SETTLE }); // the boot recovery of the staging folder
      }
      for (let i = 0; i < 100 && (await mode.callback(call)) === undefined; i++) await rig.runtime.ui.wait({ ms: 100, why: SETTLE });
      const notes: string[] = [];
      const opener = mode.opener(call, { stepInstructions: options.stepInstructions ?? STEP, zlp: options.zlp });
      const choice = await chooseBulk(dk, opener, (l) => notes.push(l), { timeoutMs: options.timeoutMs ?? 20_000, resyncMs: 200, chunk: options.chunk, log: (l) => notes.push(l) });
      const dbg = choice ? '' : JSON.stringify(await rpc(rig.session, 'usb.bulk.out', {}).then((r) => (r as { status: unknown }).status)) + ' module ' + Buffer.from((await dk.request(0x37)).body).toString('hex');
      expect(choice, `${dbg} a bulk link is negotiated (${JSON.stringify(options)}, connect ${++connects}: ${notes.join('; ')})`).toBeDefined();
      return choice!;
    };
    /** The firmware's active USB port and its port table, by their anchors. */
    const usbAnchors = () => {
      const anchor = (name: string) => {
        const a = (portFile(device, FIRMWARE[device]).anchor ?? []).filter((x) => x.name === name);
        expect(a.length, name).toBe(1);
        return a[0]!.addr as number;
      };
      return { port: anchor('cf.usb.value.active_port'), table: anchor('cf.usb.data.port_table') };
    };
    /** A menu-style change of the mode: the active port and the firmware's pointer to its table entry (the word below the port), then the bus reset of the new
     * enumeration (the connect callback of the new port's entry, when the guest has one), as the host sees a device that leaves and comes back. */
    const setPort = async (value: number) => {
      const a = usbAnchors();
      await rpc(rig.session, 'mem.cf.poke', { addr: a.port - 4, value: a.table + value * 0x28 });
      await rpc(rig.session, 'mem.cf.poke', { addr: a.port, value });
      if (value !== 2 && value !== 5) return;
      const connect = await (value === 5 ? EmuOutOnlyLink : EmuBulkLink).connectCallback(call);
      if (connect !== undefined) await rpc(rig.session, 'usb.bulk.in', { connect });
    };
    /** Boot, put the device in the mode's USB port (the boot is in USB MIDI) and negotiate the link. */
    const boot = async (mode: Mode = PAIR, name = 'uf-base') => {
      const dk = await rig.pausedBoot(await baseCard(name));
      let restarts = 0;
      if (mode.port !== 2) {
        await rig.runtime.ui.wait({ ms: 2500, why: SETTLE }); // the boot's enable first
        restarts = await word(rig.session, planSymbol(rig.plan, 'usb_bulk_restarts'));
        await setPort(mode.port);
      }
      const bulk = await connect(dk, { mode });
      // an OUT-only session starts with no stop and restart of the firmware's USB (the boot's enable in USB MIDI mode did its one)
      if (mode.outOnly) expect(await word(rig.session, planSymbol(rig.plan, 'usb_bulk_restarts')), 'the OUT-only session does not restart the USB').toBe(restarts);
      return { dk, bulk };
    };
    const elapsed = async () => Number((await rig.session.status()).elapsed);

    test.each(MODES)('$label: uploads reach the DN2 store or the DT2 +Drive with their hash; INFO carries the nonce and the replies-over-SysEx flag; VERIFY is reserved; REPLACE and E_EXISTS', async (mode) => {
      const { dk, bulk } = await boot(mode);
      const before = (await state(dk, 'uf0')).files;
      expect(before.size, 'the seed files are there').toBeGreaterThanOrEqual(2);
      const info = await bulk.session.info();
      expect([info.version, info.dataMax, info.align, info.nonce.length]).toEqual([1, 65536, 512, 8]);
      expect(info.flags & 1, 'INFO flags bit 0: replies over SysEx only in OUT-only mode').toBe(mode.outOnly ? 1 : 0);

      // a file of several 64 KiB frames, a tail that is not a multiple of 512, and a one-byte file
      const big = pattern(200_003);
      expect(await bulk.session.put(SAMPLES, rp('Kits/big.bin'), big, { expected: hashU32(big, dt2) }), 'END returns the device hash').toBe(hashU32(big, dt2));
      const one = Buffer.from([0x5a]);
      expect(await bulk.session.put(SAMPLES, rp('one.bin'), one, { expected: hashU32(one, dt2) })).toBe(hashU32(one, dt2));
      let s = await state(dk, 'uf1');
      await expectFile(dk, s, 'Kits/big.bin', big, 'big');
      await expectFile(dk, s, 'one.bin', one, 'one byte');
      unchanged(before, s.files, ['Kits/big.bin', 'one.bin']);

      await expect(bulk.session.verify(0, 100), 'VERIFY is reserved').rejects.toMatchObject({ code: BULK_STATUS.E_PROTO });

      // no REPLACE: the existing file is E_EXISTS and stays as it was
      const clash = await bulk.session.put(SAMPLES, rp('old.bin'), pattern(900)).catch((e) => e);
      expect(clash, 'E_EXISTS').toBeInstanceOf(BulkDeviceError);
      expect((clash as BulkDeviceError).code).toBe(E_EXISTS);
      s = await state(dk, 'uf2');
      expect(s.files.get('old.bin'), 'the old file is as it was').toEqual(before.get('old.bin'));
      // a wrong expected hash is E_HASH at END and leaves nothing
      const bad = await bulk.session.put(SAMPLES, rp('bad.bin'), pattern(3000), { expected: hashU32(pattern(3000, 1), dt2) }).catch((e) => e);
      expect((bad as BulkDeviceError).code, 'E_HASH').toBe(BULK_STATUS.E_HASH);
      expect((await state(dk, 'uf3')).files.has('bad.bin')).toBe(false);

      // REPLACE swaps the file: the new bytes verify
      const fresh = pattern(9000, 9);
      await bulk.session.put(SAMPLES, rp('old.bin'), fresh, { expected: hashU32(fresh, dt2), replace: true });
      s = await state(dk, 'uf4');
      await expectFile(dk, s, 'old.bin', fresh, 'replaced');
      unchanged(before, s.files, ['Kits/big.bin', 'one.bin', 'old.bin']);
      await rig.session.expectNoFault('usb-fast uploads');
    }, 1_800_000);

    test.each(MODES)('$label: a batch of files sent with DEFER is verified at each END, named by one COMMIT (DN2; the DT2 +Drive names them at END), and a deferred path can be replaced', async (mode) => {
      const { dk, bulk } = await boot(mode);
      const before = (await state(dk, 'ub0')).files;
      const batch: [string, Buffer][] = [
        ['batch/a.bin', pattern(3000, 1)],
        ['batch/b.bin', pattern(700, 2)],
        ['batch/c.bin', pattern(70_001, 3)],
        ['batch/d.bin', pattern(512, 4)],
        ['batch/e.bin', pattern(12_345, 5)],
        ['batch/f.bin', Buffer.from([0x33])],
      ];
      const pending: (number | undefined)[] = [];
      for (const [name, data] of batch) {
        const r = await bulk.session.put(SAMPLES, rp(name), data, { expected: hashU32(data, dt2), defer: true });
        expect(r.hash, `${name}: END returns the device hash`).toBe(hashU32(data, dt2));
        pending.push(r.pending);
      }
      // a path that is already pending is replaced by the next file of that path (DN2: the pending entry; DT2: the +Drive replace)
      const first = pattern(2048, 6);
      const second = pattern(4096, 7);
      await bulk.session.put(SAMPLES, rp('batch/dup.bin'), first, { expected: hashU32(first, dt2), defer: true });
      const last = await bulk.session.put(SAMPLES, rp('batch/dup.bin'), second, { expected: hashU32(second, dt2), defer: true, replace: true });
      pending.push(last.pending);
      if (dt2) {
        expect(pending, 'the +Drive names its files at END: nothing is ever pending').toEqual(pending.map(() => 0));
      } else {
        expect(pending.slice(0, batch.length), 'the DN2 holds each verified file pending').toEqual(batch.map((_, i) => i + 1));
        expect(last.pending, 'the replaced path took the place of the pending file').toBe(batch.length + 1);
        const hidden = (await state(dk, 'ub1')).files;
        expect([...hidden.keys()].filter((n) => n.startsWith('batch/')), 'a pending file is not listed').toEqual([]);
      }
      expect(await bulk.session.commit(), 'COMMIT names them all').toBe(0);
      expect(await bulk.session.commit(), 'a second COMMIT has nothing to do').toBe(0);
      const s = await state(dk, 'ub2');
      for (const [name, data] of batch) await expectFile(dk, s, name, data, name);
      await expectFile(dk, s, 'batch/dup.bin', second, 'the replaced pending path');
      unchanged(before, s.files, [...batch.map(([n]) => n), 'batch/dup.bin']);

      await rig.session.expectNoFault('usb-fast batch');
    }, 1_800_000);

    test.each(MODES)('$label: copy/write overlap: the same device hash and the same card contents for files below, at and above the 512 KiB half, with and without DEFER', async (mode) => {
      const { dk, bulk } = await boot(mode);
      // sizes: one half and less (one buffer), a tail in the second half, exact halves, a tail that is not a multiple of 512, several flushes
      const files: [string, Buffer][] = [
        ['small', pattern(300_001, 21)],
        ['half', pattern(524_288, 22)],
        ['half-plus', pattern(524_288 + 70_001, 23)],
        ['one-mib', pattern(1_048_576, 24)],
        ['odd', pattern(1_000_003, 25)],
        ['several', pattern(3 * 1_048_576 + 700, 26)],
      ];
      for (const [name, data] of files) {
        const want = hashU32(data, dt2);
        expect(await bulk.session.put(SAMPLES, rp(`Ov/${name}`), data, { expected: want }), `${name}: END returns the device hash`).toBe(want);
      }
      // a batch: deferred files across the half size are verified at their END and named by one COMMIT
      for (const [name, data] of files.slice(2, 4)) {
        const r = await bulk.session.put(SAMPLES, rp(`Ov/d-${name}`), data, { expected: hashU32(data, dt2), defer: true });
        expect(r.hash, `deferred ${name}`).toBe(hashU32(data, dt2));
      }
      expect(await bulk.session.commit(), 'COMMIT').toBe(0);
      const s = await state(dk, 'ov0');
      for (const [name, data] of files) await expectFile(dk, s, `Ov/${name}`, data, name);
      for (const [name, data] of files.slice(2, 4)) await expectFile(dk, s, `Ov/d-${name}`, data, `deferred ${name}`);
      await rig.session.expectNoFault('usb-fast overlap');
    }, 3_600_000);

    /** STATUS 'S' [bit [0|1]]: the four speed switches (bit 0 hash once, bit 1 the USB receive ring, bit 2 the overlapped read-back hash, bit 3 large card commands); the reply is the mask now. */
    const speedMask = async (dk: DkSession, bit?: number, on?: number) =>
      Buffer.from((await dk.request(0x37, Uint8Array.from(bit === undefined ? [0x53] : [0x53, bit, on!]))).body).readUInt32BE(0);

    test('the speed switches (hash once, overlapped read-back hash), on and off: the device hash is the host hash, and a corrupted read-back or a wrong hash refuses the commit', async () => {
      const data = pattern(300_011, 17); // more than four 64 KiB pieces (DN2) and nine 32 KiB pages (DT2)
      const more = pattern(70_001, 19);
      const cases: [string, number, number][] = [['both on (the default)', 1, 1], ['hash once off', 0, 1], ['overlap off', 1, 0], ['both off', 0, 0]];
      for (const [k, [label, a, c]] of cases.entries()) {
        const dk = await rig.pausedBoot(await baseCard());
        const bulk = await connect(dk);
        expect(await speedMask(dk), `${label}: all switches are on after a boot`).toBe(15);
        await speedMask(dk, 0, a);
        expect(await speedMask(dk, 2, c), `${label}: the mask`).toBe((a ? 1 : 0) | 2 | (c ? 4 : 0) | 8);
        // a corrupted read-back (DN2: the card holds another byte in its first write) or a wrong hash (DT2) refuses the commit
        if (dt2) {
          const bad = await bulk.session.put(SAMPLES, rp('Sw/bad.bin'), data, { expected: hashU32(pattern(300_011, 18), dt2) }).catch((e) => e);
          expect((bad as BulkDeviceError).code, `${label}: a wrong hash`).toBe(BULK_STATUS.E_HASH);
        } else {
          // the first card write: with core/sdhc-large on, the 300 KB file is one write (the third write would be the table commit)
          await pokeKnob(rig.session, rig.plan, 'DKRT_UPLOAD_FLIP', 1);
          const bad = await bulk.session.put(SAMPLES, rp('Sw/bad.bin'), data, { expected: hashU32(data, dt2) }).catch((e) => e);
          expect(bad, `${label}: a flipped byte`).toBeInstanceOf(BulkDeviceError);
          expect((bad as BulkDeviceError).code, `${label}: the hash error`).toBe(BULK_STATUS.E_HASH);
        }
        // no host hash (the received bytes are hashed whatever the switch says), then with it
        expect(await bulk.session.put(SAMPLES, rp('Sw/plain.bin'), data), `${label}: END returns the device hash`).toBe(hashU32(data, dt2));
        expect(await bulk.session.put(SAMPLES, rp('Sw/hashed.bin'), more, { expected: hashU32(more, dt2) }), `${label}: END with a host hash`).toBe(hashU32(more, dt2));
        const s = await state(dk, `sw${k}`);
        expect(s.files.has('Sw/bad.bin'), `${label}: nothing of the refused file`).toBe(false);
        await expectFile(dk, s, 'Sw/plain.bin', data, `${label}: plain`);
        await expectFile(dk, s, 'Sw/hashed.bin', more, `${label}: hashed`);
        await rig.session.expectNoFault(`usb-fast switches ${label}`);
      }
    }, 3_600_000);

    // DN2 only (the knob counts the store writer's card writes). Coverage of the other card faults: upload-faults.device.test.ts (core/runtime-tests) flips each of the
    // three data writes of a small upload (E_HASH, nothing committed) and cuts or tears every write of the commit; this test adds the usb-fast geometry (two data writes, with the ring and large commands on and off) and the flipped table commit.
    test('DN2: a flipped byte in the second data write is E_HASH, and a flipped table-commit write is E_WRITE; nothing of the file is committed either way', async () => {
      if (dt2) return;
      const data = pattern(300_011, 23);
      // the 300 KB file is two card writes (the 256 KiB staging flush and the tail), the third write is the table commit
      const cases: [string, number, number, number][] = [
        ['ring and large commands off: the second data write', 0, 2, BULK_STATUS.E_HASH],
        ['ring and large commands on: the second data write', 1, 2, BULK_STATUS.E_HASH],
        ['ring and large commands off: the table commit', 0, 3, BULK_STATUS.E_WRITE],
        ['ring and large commands on: the table commit', 1, 3, BULK_STATUS.E_WRITE],
      ];

      for (const [k, [label, large, write, code]] of cases.entries()) {
        const dk = await rig.pausedBoot(await baseCard());
        const bulk = await connect(dk);
        await speedMask(dk, 1, large);
        expect(await speedMask(dk, 3, large), `${label}: the mask`).toBe(large ? 15 : 5);
        await pokeKnob(rig.session, rig.plan, 'DKRT_UPLOAD_FLIP', write);
        const bad = await bulk.session.put(SAMPLES, rp('Fl/bad.bin'), data, { expected: hashU32(data, dt2) }).catch((e) => e);
        expect(bad, `${label}: refused`).toBeInstanceOf(BulkDeviceError);
        expect((bad as BulkDeviceError).code, label).toBe(code);
        await pokeKnob(rig.session, rig.plan, 'DKRT_UPLOAD_FLIP', 0);
        expect(await bulk.session.put(SAMPLES, rp('Fl/ok.bin'), data, { expected: hashU32(data, dt2) }), `${label}: the next upload works`).toBe(hashU32(data, dt2));
        const s = await state(dk, `fl${k}`);
        expect(s.files.has('Fl/bad.bin'), `${label}: nothing of the refused file`).toBe(false);
        await expectFile(dk, s, 'Fl/ok.bin', data, `${label}: ok.bin`);
        await rig.session.expectNoFault(`usb-fast flip ${label}`);
      }
    }, 3_600_000);

    test.each(MODES)('$label: ABORT drops the open file and a cancel mid-transfer leaves the old file; a USB reset in the middle of a file leaves nothing and the next upload works', async (mode) => {
      const { dk, bulk } = await boot(mode);
      const before = (await state(dk, 'ua0')).files;
      await bulk.session.abort(); // nothing open: ok

      // a cancel after the first acknowledged frames: ABORT is sent, the destination is not created; a replace keeps the old file
      const half = pattern(400_000, 2);
      for (const [name, replace] of [['aborted.bin', false], ['old.bin', true]] as const) {
        const stop = new AbortController();
        const e = await bulk.session
          .put(SAMPLES, rp(name), half, { replace, signal: stop.signal, onProgress: (sent) => sent >= 130_000 && stop.abort() })
          .catch((x) => x);
        expect((e as { kind?: string }).kind, `${name}: cancelled`).toBe('cancelled');
      }
      let s = await state(dk, 'ua1');
      expect(s.files.has('aborted.bin'), 'nothing at the aborted path').toBe(false);
      unchanged(before, s.files, []);
      const ok = pattern(20_000, 4);
      await bulk.session.put(SAMPLES, rp('aborted.bin'), ok, { expected: hashU32(ok, dt2) });
      s = await state(dk, 'ua2');
      await expectFile(dk, s, 'aborted.bin', ok, 'after the aborts');

      // a bus reset while the file is open (the host's request is lost): the module aborts the file and clears the parser; the host fails the upload and resyncs
      const reset = await connect(dk, { timeoutMs: 1500, mode });
      const big = pattern(600_000, 6);
      const connectAddr = await mode.callback(call);
      let cut = false;
      const failed = await reset.session
        .put(SAMPLES, rp('reset.bin'), big, {
          replace: false,
          onProgress: (sent) => {
            if (!cut && sent >= 200_000) {
              cut = true;
              void rpc(rig.session, 'usb.bulk.in', { connect: connectAddr });
            }
          },
        })
        .catch((e) => e);
      expect(cut, 'the reset was delivered mid-file').toBe(true);
      expect(failed, 'the upload fails').toBeInstanceOf(Error);
      s = await state(dk, 'ua3');
      expect(s.files.has('reset.bin'), 'no partial file').toBe(false);
      expect([...s.files.keys()].sort(), 'the files are those of before and the one before the reset').toEqual([...before.keys(), 'aborted.bin'].sort());
      // the host starts again on a fresh link: the same destination takes the file, so no session was left open
      const again = await connect(dk, { mode });
      await again.session.put(SAMPLES, rp('reset.bin'), ok, { expected: hashU32(ok, dt2) });
      s = await state(dk, 'ua4');
      await expectFile(dk, s, 'reset.bin', ok, 'after the reset');
      await rig.session.expectNoFault('usb-fast abort and reset');
    }, 1_800_000);

    test.each(MODES)('$label: frames that span receive buffers and buffers with many frames, around 16 KiB and the 512-byte packet, with and without the zero-length packet', async (mode) => {
      const { dk } = await boot(mode);
      const before = (await state(dk, 'uz0')).files;
      // chunk sizes: 512 (528-byte frames, about 31 to a 16 KiB buffer), 16 KiB (a frame is 16 bytes over a buffer), 65536 (a frame spans five buffers). File sizes: the last
      // DATA frame is 12 + 4 + n bytes: n = 496 and 16368 make a frame of exactly 512 and 16384 bytes (the zero-length packet ends it; 16384 fills a buffer
      // and the packet gets a dTD of its own), 16369 and 16385 are one byte over.
      const sizes = [1, 496, 497, 511, 512, 1000, 16_368, 16_369, 16_384, 16_385, 16_368 + 512 * 3, 65_536, 65_536 + 496, 100_000];
      let k = 0;
      for (const chunk of [512, 16_384, 65_536]) {
        const { session } = await connect(dk, { chunk, mode });
        for (const size of sizes) {
          const data = pattern(size, size + chunk);
          const got = await session.put(SAMPLES, rp(`f${chunk}-${size}.bin`), data, { expected: hashU32(data, dt2) });
          expect(got, `chunk ${chunk}, ${size} bytes: END hash`).toBe(hashU32(data, dt2));
          k++;
        }
      }
      const s = await state(dk, 'uz1');
      expect(s.files.size, 'every file is there').toBe(before.size + k);
      for (const [name, size, chunk] of [['f512-496.bin', 496, 512], ['f16384-16368.bin', 16_368, 16_384], ['f65536-65536.bin', 65_536, 65_536], ['f512-100000.bin', 100_000, 512]] as const)
        await expectFile(dk, s, name, pattern(size, size + chunk), name);

      // without the zero-length packet the device never sees a frame of exactly 512 bytes: no reply; the packet completes it
      const raw = (await connect(dk, { zlp: false, mode })).link;
      const rq = (cmd: number, seq: number, body: Buffer) => raw.send(bulkRequest(cmd, seq, body));
      const be32 = (n: number) => Buffer.from([n >>> 24, (n >>> 16) & 255, (n >>> 8) & 255, n & 255]);
      const data = pattern(496, 77);
      await rq(BULK_CMD.BEGIN, 1, Buffer.concat([be32(SAMPLES), Buffer.from(`${rp('nozlp.bin')}\0`), be32(data.length), Buffer.alloc(4)]));
      expect(parseBulkReply((await raw.recv(20_000))!).status, 'BEGIN').toBe(0);
      await rq(BULK_CMD.DATA, 2, Buffer.concat([be32(0), data])); // the frame is 512 bytes: no packet follows
      for (let i = 0; i < 40; i++) await rig.runtime.ui.wait({ ms: 50, why: SETTLE });
      expect(await raw.recv(300), 'no reply while the transfer is open').toBeNull();
      await rpc(rig.session, 'usb.bulk.in', { zlp: true });
      const r = parseBulkReply((await raw.recv(20_000))!);
      expect([r.cmd, r.seq, r.status], 'the packet ends the transfer: DATA is answered').toEqual([BULK_CMD.DATA, 2, 0]);
      await rq(BULK_CMD.END, 3, Buffer.alloc(0));
      await rpc(rig.session, 'usb.bulk.in', { zlp: true }); // END is 12 bytes: this packet is an extra empty transfer the adapter drops
      const end = parseBulkReply((await raw.recv(20_000))!);
      expect([end.status, Buffer.from(end.body).readUInt32BE(0)], 'END').toEqual([0, hashU32(data, dt2)]);
      await rig.session.expectNoFault('usb-fast frames');
    }, 3_600_000);

    test('a nonce that does not match makes the host stay on SysEx; a bad stream is answered E_MSG and the link works again', async () => {
      const { dk } = await boot();
      const notes: string[] = [];
      const wrong = { usbFastNonce: (n: Uint8Array) => dk.usbFastNonce(n.map((b) => b ^ 0xff)) };
      const none = await chooseBulk(wrong, EmuBulkLink.opener(call, { stepInstructions: STEP }), (l) => notes.push(l), { timeoutMs: 20_000 });
      expect(none, 'no bulk link: the host stays on SysEx').toBeUndefined();
      expect(notes.join('\n')).toMatch(/other nonce|none of 1 candidate/);
      // SysEx works as before
      const f = pattern(3000, 8);
      await dk.put(dt2 ? 'data/sysex.bin' : 'samples/sysex.bin', f);

      await rig.session.expectNoFault('usb-fast nonce');
    }, 1_800_000);

    /** One request frame; the replies of the module as they come (in order). */
    const be32 = (n: number) => Buffer.from([n >>> 24, (n >>> 16) & 255, (n >>> 8) & 255, n & 255]);
    const beginBody = (name: string, size: number, expected?: Buffer) =>
      Buffer.concat([be32(SAMPLES), Buffer.from(`${rp(name)}\0`), be32(size), expected ?? Buffer.alloc(4)]);
    const req = (cmd: number, seq: number, body: Buffer = Buffer.alloc(0), flags = 0) => Buffer.from(bulkRequest(cmd, seq, body, flags));
    const dataFrame = (seq: number, offset: number, bytes: Buffer) => req(BULK_CMD.DATA, seq, Buffer.concat([be32(offset), bytes]));
    const replies = async (link: BulkChoice['link'], n: number) => {
      const out = [];
      for (let i = 0; i < n; i++) {
        const f = await link.recv(20_000);
        expect(f, `reply ${i + 1} of ${n}`).not.toBeNull();
        const r = parseBulkReply(f!);
        out.push({ cmd: r.cmd, seq: r.seq, status: r.status, body: Buffer.from(r.body) });
      }
      return out;
    };
    const quiet = async (link: BulkChoice['link'], what: string) => {
      for (let i = 0; i < 20; i++) await rig.runtime.ui.wait({ ms: 50, why: SETTLE });
      expect(await link.recv(300), what).toBeNull();
    };
    const moduleStatus = async (dk: DkSession) => {
      const b = Buffer.from((await dk.request(0x37)).body);
      return { frames: b.readUInt32BE(4), fatal: b.readUInt32BE(48), resets: b.readUInt32BE(52), enable: b.readInt32BE(56), sendErr: b.readUInt32BE(60) };
    };

    test('the parser: several frames in one transfer, buffers that end inside a header or a body, a corrupt stream and a bad request in every state', async () => {
      const { dk, bulk } = await boot();
      const link = bulk.link;
      expect((await moduleStatus(dk)).enable, 'usb_bulk_enable succeeded').toBe(0);
      const a = pattern(3 * 512, 61);
      const b = pattern(1000, 62);
      const stable = (x: Buffer) => be32(hashU32(x, dt2));

      // BEGIN, two DATA and END in ONE transfer: four replies, in order
      await link.send(
        Buffer.concat([
          req(BULK_CMD.BEGIN, 10, beginBody('p-one.bin', a.length, stable(a))),
          dataFrame(11, 0, a.subarray(0, 1024)),
          dataFrame(12, 1024, a.subarray(1024)),
          req(BULK_CMD.END, 13),
        ]),
      );
      let r = await replies(link, 4);
      expect(r.map((x) => [x.cmd, x.seq, x.status]), 'one transfer, four frames, four replies in order').toEqual([
        [BULK_CMD.BEGIN, 10, 0],
        [BULK_CMD.DATA, 11, 0],
        [BULK_CMD.DATA, 12, 0],
        [BULK_CMD.END, 13, 0],
      ]);
      expect(r[3]!.body.readUInt32BE(0), 'END hash').toBe(hashU32(a, dt2));

      // END of one file, BEGIN and DATA of the next, in one transfer; its END alone
      await link.send(req(BULK_CMD.BEGIN, 20, beginBody('p-two.bin', b.length, stable(b))));
      await replies(link, 1);
      await link.send(
        Buffer.concat([
          dataFrame(21, 0, b),
          req(BULK_CMD.END, 22),
          req(BULK_CMD.BEGIN, 23, beginBody('p-three.bin', a.length, stable(a))),
          dataFrame(24, 0, a),
        ]),
      );
      r = await replies(link, 4);
      expect(r.map((x) => [x.cmd, x.seq, x.status]), 'END, BEGIN and DATA behind it').toEqual([
        [BULK_CMD.DATA, 21, 0],
        [BULK_CMD.END, 22, 0],
        [BULK_CMD.BEGIN, 23, 0],
        [BULK_CMD.DATA, 24, 0],
      ]);
      await link.send(req(BULK_CMD.END, 25));
      expect((await replies(link, 1))[0]!.status).toBe(0);

      // a transfer that ends inside the 12-byte header (5, 12 and 13 bytes in) and inside a body: the module joins the pieces
      const cuts: [string, Buffer, number][] = [
        ['inside the header (5 bytes)', req(BULK_CMD.BEGIN, 30, beginBody('p-cut5.bin', a.length, stable(a))), 5],
        ['at the end of the header (12 bytes)', dataFrame(31, 0, a), 12],
        ['one byte into the body (13 bytes)', dataFrame(32, 0, a), 13],
        ['inside a body', dataFrame(33, 0, a), 700],
      ];
      for (const [label, frame, at] of cuts) {
        if (frame[4] === BULK_CMD.BEGIN) {
          await link.send(frame.subarray(0, at));
          await link.send(frame.subarray(at));
        } else {
          await link.send(req(BULK_CMD.BEGIN, 40 + frame[5]!, beginBody(`p-${frame[5]}.bin`, a.length, stable(a))));
          expect((await replies(link, 1))[0]!.status, `${label}: BEGIN`).toBe(0);
          await link.send(frame.subarray(0, at));
          await link.send(frame.subarray(at));
        }
        const got = await replies(link, 1);
        expect([got[0]!.seq, got[0]!.status], `${label}: the frame in two transfers is one request, one reply`).toEqual([frame[5], 0]);
        if (frame[4] === BULK_CMD.BEGIN) await link.send(dataFrame(34, 0, a));
        else await link.send(req(BULK_CMD.END, 50 + frame[5]!));
        const next = (await replies(link, 1))[0]!;
        expect(next.status, `${label}: the file goes on`).toBe(0);
        if (frame[4] === BULK_CMD.BEGIN) {
          await link.send(req(BULK_CMD.END, 35));
          expect((await replies(link, 1))[0]!.status).toBe(0);
        }
      }
      let s = await state(dk, 'ps0');
      for (const [name, x] of [['p-one.bin', a], ['p-two.bin', b], ['p-three.bin', a], ['p-cut5.bin', a], ['p-31.bin', a], ['p-32.bin', a], ['p-33.bin', a]] as const)
        await expectFile(dk, s, name, x, name);

      // requests in the wrong state: DATA without BEGIN, a wrong offset (the file stays open), END before all bytes (closes it, nothing stored), a double END
      await link.send(dataFrame(60, 0, a));
      expect((await replies(link, 1))[0], 'DATA without BEGIN').toMatchObject({ seq: 60, status: BULK_STATUS.E_STATE });
      await link.send(req(BULK_CMD.BEGIN, 61, beginBody('p-state.bin', a.length, stable(a))));
      await replies(link, 1);
      await link.send(dataFrame(62, 512, a.subarray(512, 1024)));
      expect((await replies(link, 1))[0], 'a wrong offset').toMatchObject({ seq: 62, status: BULK_STATUS.E_OFFSET });
      await link.send(dataFrame(63, 0, a.subarray(0, 512)));
      expect((await replies(link, 1))[0]!.status, 'the file is still open: the right offset works').toBe(0);
      await link.send(req(BULK_CMD.END, 64));
      expect((await replies(link, 1))[0], 'END before all bytes').toMatchObject({ seq: 64, status: BULK_STATUS.E_LENGTH });
      await link.send(req(BULK_CMD.END, 65));
      expect((await replies(link, 1))[0], 'a second END').toMatchObject({ seq: 65, status: BULK_STATUS.E_STATE });
      s = await state(dk, 'ps1');
      expect(s.files.has('p-state.bin'), 'nothing stored for the short file').toBe(false);

      // BEGIN while a file is open: the old file is aborted, the new one opens
      await link.send(req(BULK_CMD.BEGIN, 70, beginBody('p-old.bin', a.length, stable(a))));
      await link.send(dataFrame(71, 0, a.subarray(0, 512)));
      await replies(link, 2);
      // (the DT2 adapter may be busy with the UI task's clean-up of an earlier file for a moment: the module answers E_BUSY and the host asks again, as dk-session does)
      let over: { seq: number; status: number } = { seq: 0, status: BULK_STATUS.L_E_BUSY };
      for (let i = 0; i < 100 && over.status === BULK_STATUS.L_E_BUSY; i++) {
        await link.send(req(BULK_CMD.BEGIN, 72, beginBody('p-new.bin', b.length, stable(b))));
        over = (await replies(link, 1))[0]!;
        if (over.status === BULK_STATUS.L_E_BUSY) await rig.runtime.ui.wait({ ms: 20, why: SETTLE });
      }
      expect(over, 'BEGIN over an open file').toMatchObject({ seq: 72, status: 0 });
      await link.send(dataFrame(73, 0, b));
      await link.send(req(BULK_CMD.END, 74));
      expect((await replies(link, 2)).map((x) => x.status)).toEqual([0, 0]);
      s = await state(dk, 'ps2');
      expect(s.files.has('p-old.bin'), 'the abandoned file is not stored').toBe(false);
      await expectFile(dk, s, 'p-new.bin', b, 'the new file');

      // a corrupt stream: an oversize length (valid magic) followed by garbage in the same transfer, with a valid INFO inside it. One E_MSG with the seq, nothing for
      // the garbage; the next transfer is parsed again.
      const garbage = Buffer.concat([
        Buffer.from('DKB1'), Buffer.from([BULK_CMD.DATA, 0x44, 0, 0]), Buffer.from([0xff, 0xff, 0xff, 0xf0]),
        pattern(300, 3), req(BULK_CMD.INFO, 0x45), pattern(100, 4), req(BULK_CMD.INFO, 0x46),
      ]);
      await link.send(req(BULK_CMD.BEGIN, 80, beginBody('p-corrupt.bin', a.length)));
      await replies(link, 1);
      await link.send(garbage);
      const e1 = await replies(link, 1);
      expect([e1[0]!.cmd, e1[0]!.seq, e1[0]!.status], 'one E_MSG, for the seq of the bad header').toEqual([BULK_CMD.DATA, 0x44, BULK_STATUS.E_MSG]);
      await quiet(link, 'no reply for the garbage after the bad header');
      await link.send(req(BULK_CMD.INFO, 0x47));
      expect((await replies(link, 1))[0], 'the next transfer is parsed').toMatchObject({ cmd: BULK_CMD.INFO, seq: 0x47, status: 0 });
      // the fatal error aborted the open file: its path is free again
      await link.send(req(BULK_CMD.BEGIN, 81, beginBody('p-corrupt.bin', b.length, stable(b))));
      expect((await replies(link, 1))[0]!.status, 'the file of the corrupt stream was aborted').toBe(0);
      await link.send(Buffer.concat([dataFrame(82, 0, b), req(BULK_CMD.END, 83)]));
      expect((await replies(link, 2)).map((x) => x.status)).toEqual([0, 0]);
      // a bad magic: no reply at all (the seq cannot be trusted), the garbage behind it in the transfer is dropped too
      await link.send(Buffer.concat([Buffer.from('XKB1'), Buffer.from([1, 0x48, 0, 0, 0, 0, 0, 0]), pattern(200, 5), req(BULK_CMD.INFO, 0x49)]));
      await quiet(link, 'a bad magic is not answered');
      await link.send(req(BULK_CMD.INFO, 0x4a));
      expect((await replies(link, 1))[0], 'and the stream goes on').toMatchObject({ seq: 0x4a, status: 0 });
      expect((await moduleStatus(dk)).fatal, 'two fatal errors counted').toBe(2);

      // a bus reset while replies are owed and a buffer is held: 40 requests in one transfer, no reply read, the reset before the module is through
      const resets = (await moduleStatus(dk)).resets;
      await link.send(Buffer.concat(Array.from({ length: 40 }, (_, i) => req(BULK_CMD.INFO, i))));
      await rig.runtime.ui.wait({ ms: 1, why: SETTLE });
      await rpc(rig.session, 'usb.bulk.in', { connect: (await EmuBulkLink.connectCallback(call))! });
      await rig.runtime.ui.wait({ ms: 200, why: SETTLE });
      while (await link.recv(300)) {
        /* drain what was answered before the reset */
      }
      expect((await moduleStatus(dk)).resets, 'the module saw the reset').toBeGreaterThan(resets);
      await link.send(req(BULK_CMD.INFO, 0x4b));
      expect((await replies(link, 1))[0], 'the link works after the reset').toMatchObject({ seq: 0x4b, status: 0 });
      await rig.session.expectNoFault('usb-fast parser');
    }, 3_600_000);

    test('a power cut in the middle of a bulk upload leaves the old file or the new one, whole, and the next upload works', async () => {
      const data = pattern(50_000, 31);
      // DN2: the number of card writes of one upload, from a boot of its own (the last write is header A, the one before it header B)
      let writes = 0;
      if (!dt2) {
        const dk = await rig.pausedBoot(await baseCard());
        const bulk = await connect(dk);
        await bulk.session.put(SAMPLES, rp('Cut/cut.bin'), data, { expected: hashU32(data, dt2) });
        writes = await word(rig.session, planSymbol(rig.plan, 'DKRT_UPLOAD_WRITES'));
        expect(writes, 'card writes of an upload').toBeGreaterThan(3);
      }
      // [label, knobs, replace, the new file is at the destination afterwards, the upload is refused with E_WRITE]
      const cases: [string, Record<string, number>, boolean, boolean, boolean][] = dt2
        ? [
            ['mid-data of a new file', { DKRT_PD_CUT: 1 }, false, false, true],
            ['mid-data of a replace', { DKRT_PD_CUT: 1 }, true, false, true],
            ['a replace between the delete of the destination and the rename', { DKRT_PD_CUT: 4 }, true, true, true],
          ]
        : [
            ['no cut', {}, true, true, false],
            ['a cut after the first card write', { DKRT_UPLOAD_CUT: 1 }, false, false, true],
            ['a tear of header B', { DKRT_UPLOAD_TEAR: writes - 1 }, true, false, true],
            ['a cut after header B (header A is refused)', { DKRT_UPLOAD_CUT: writes - 1 }, true, true, true],
          ];
      for (const [k, [label, knobs, replace, fresh, refused]] of cases.entries()) {
        const target = replace ? 'old.bin' : 'Cut/cut.bin';
        const dk = await rig.pausedBoot(await baseCard());
        const bulk = await connect(dk, { timeoutMs: 20_000 });
        const before = (await state(dk, `pc${k}a`)).files;
        for (const [name, value] of Object.entries(knobs)) await pokeKnob(rig.session, rig.plan, name, value);
        const r = await bulk.session.put(SAMPLES, rp(target), data, { replace, expected: hashU32(data, dt2) }).catch((e) => e);
        if (refused) {
          expect(r, `${label}: the upload fails`).toBeInstanceOf(BulkDeviceError);
          expect((r as BulkDeviceError).code, `${label}: with the firmware's write error`).toBe(BULK_STATUS.E_WRITE);
        } else expect(r, `${label}: no cut: the upload works`).toBe(hashU32(data, dt2));
        const cutCard = await rig.after(`pc${k}b`);
        const next = await rig.pausedBoot(cutCard);
        const bulk2 = await connect(next);
        const s = await state(next, `pc${k}c`);
        if (fresh) await expectFile(next, s, target, data, `${label}: the new file`);
        else if (replace) expect(s.files.get(target), `${label}: the old file`).toEqual(before.get(target));
        else if (dt2) {
          // a new file is written at its final name (core/plusdrive-write, no boot cleanup): at most that one file stays, without its hash
          const left = s.files.get(target);
          if (left) expect(left.hash, `${label}: the partial file is unhashed`).not.toBe(hex(data, true).slice(0, 8));
        } else expect(s.files.has(target), `${label}: nothing at the destination`).toBe(false);
        unchanged(before, s.files, [target, 'Cut/cut.bin']);
        // the next upload works, and the destination is as before it
        const more = pattern(9000, 41);
        await bulk2.session.put(SAMPLES, rp('after.bin'), more, { expected: hashU32(more, dt2) });
        const t = await state(next, `pc${k}d`);
        await expectFile(next, t, 'after.bin', more, `${label}: the next upload`);
        if (dt2 && !replace && !fresh) {
          // the next upload of the same name replaces the partial file and verifies
          await bulk2.session.put(SAMPLES, rp(target), data, { replace: true, expected: hashU32(data, dt2) });
          await expectFile(next, await state(next, `pc${k}e`), target, data, `${label}: the re-upload`);
        } else expect(t.files.get(target), `${label}: the destination is unchanged by it`).toEqual(s.files.get(target));
      }
    }, 3_600_000);

    /** STATUS 'A' [limit]: sets the largest staging buffer of the next BEGIN (0: none) and returns the size the last BEGIN got. */
    const accKnob = async (dk: DkSession, limit?: number) => {
      const body = limit === undefined ? [0x41] : [0x41, ...be32(limit)];
      return Buffer.from((await dk.request(0x37, Uint8Array.from(body))).body).readUInt32BE(0);
    };

    test('the staging buffer: sizes around its edges, abort and bus reset across a flush, and a small or no buffer, all with the device hash equal to the host hash', async () => {
      const { dk, bulk } = await boot();
      const MIB = 1 << 20;
      // STATUS 'A' exists only in a module built with UF_KNOBS 1 (src/c/uf.h); the default build answers an unknown tag with E_MSG: then the sizes run with the buffer
      // the module chooses, and the forced small and no-buffer cases are skipped
      const probe = await dk.request(0x37, Uint8Array.from([0x41]));
      const knobs = probe.status === 0 && probe.body.length === 4;
      // 0 and 1, a sector short, 64 KiB, one byte under, at and over one buffer, and three buffers with a tail
      const sizes = [0, 1, 511, 65_536, MIB - 1, MIB, MIB + 512, 3 * MIB + 777];
      const obtained: Record<string, number> = {};
      const run = async (session: BulkChoice['session'], tag: string, list: number[], limit?: number) => {
        for (const size of list) {
          if (limit !== undefined && knobs) await accKnob(dk, limit); // the knob lasts one BEGIN
          const data = pattern(size, size + 3);
          const got = await session.put(SAMPLES, rp(`acc-${tag}-${size}.bin`), data, { expected: hashU32(data, dt2) });
          expect(got, `${tag} ${size} bytes: the device hash is the host hash`).toBe(hashU32(data, dt2));
          obtained[`${tag}-${size}`] = knobs ? await accKnob(dk) : 0;
        }
      };
      await run(bulk.session, 'big', sizes);
      let s = await state(dk, 'ub0');
      for (const size of [511, MIB + 512, 3 * MIB + 777]) await expectFile(dk, s, `acc-big-${size}.bin`, pattern(size, size + 3), `${size} bytes`);

      if (knobs) {
        // a forced small buffer (64 KiB), then none (one file_write per DATA, as before): the same bytes arrive
        expect(await accKnob(dk, 65_536), 'the knob returns the size of the last BEGIN').toBe(obtained[`big-${3 * MIB + 777}`]);
        await run(bulk.session, 'small', [1, 65_536 + 512, MIB + 5], 65_536);
        expect(obtained[`small-${MIB + 5}`], 'a forced 64 KiB buffer').toBe(65_536);
        await run(bulk.session, 'none', [511, 200_003], 0);
        expect(obtained['none-200003'], 'no buffer').toBe(0);
        s = await state(dk, 'ub1');
        await expectFile(dk, s, `acc-small-${MIB + 5}.bin`, pattern(MIB + 5, MIB + 5 + 3), 'small buffer');
        await expectFile(dk, s, 'acc-none-200003.bin', pattern(200_003, 200_003 + 3), 'no buffer');

      }
      // the protocol rules of file_write stay: a wrong offset, a part-sector DATA that is not the last, bytes past the size (none of them takes the file down)
      const link = bulk.link;
      const a = pattern(4096, 91);
      await link.send(req(BULK_CMD.BEGIN, 1, beginBody('acc-rules.bin', a.length, be32(hashU32(a, dt2)))));
      await replies(link, 1);
      await link.send(dataFrame(2, 512, a.subarray(512, 1024)));
      await link.send(dataFrame(3, 0, a.subarray(0, 100)));
      await link.send(dataFrame(4, 0, Buffer.concat([a, Buffer.alloc(1)])));
      expect((await replies(link, 3)).map((x) => x.status), 'offset, length, past the size').toEqual([BULK_STATUS.E_OFFSET, BULK_STATUS.E_LENGTH, BULK_STATUS.E_LENGTH]);
      await link.send(Buffer.concat([dataFrame(5, 0, a.subarray(0, 1024)), dataFrame(6, 1024, a.subarray(1024)), req(BULK_CMD.END, 7)]));
      const rr = await replies(link, 3);
      expect(rr.map((x) => x.status), 'the file goes on and ends').toEqual([0, 0, 0]);
      expect(rr[2]!.body.readUInt32BE(0)).toBe(hashU32(a, dt2));

      // a cancel after the first buffer was flushed: the destination is not created; a bus reset after it leaves no file and the next upload works
      const before = (await state(dk, 'ub2')).files;
      const long = pattern(3 * MIB, 7);
      const stop = new AbortController();
      const e = await bulk.session
        .put(SAMPLES, rp('acc-cancel.bin'), long, { signal: stop.signal, onProgress: (sent) => sent >= MIB + MIB / 2 && stop.abort() })
        .catch((x) => x);
      expect((e as { kind?: string }).kind, 'cancelled after a flush').toBe('cancelled');
      expect((await state(dk, 'ub3')).files.has('acc-cancel.bin'), 'nothing at the aborted path').toBe(false);
      const reset = await connect(dk, { timeoutMs: 1500 });
      const connectAddr = await EmuBulkLink.connectCallback(call);
      let cut = false;
      const failed = await reset.session
        .put(SAMPLES, rp('acc-reset.bin'), long, {
          onProgress: (sent) => {
            if (!cut && sent >= MIB + MIB / 4) {
              cut = true;
              void rpc(rig.session, 'usb.bulk.in', { connect: connectAddr });
            }
          },
        })
        .catch((x) => x);
      expect(cut, 'the reset came after a flush').toBe(true);
      expect(failed, 'the upload fails').toBeInstanceOf(Error);
      s = await state(dk, 'ub4');
      expect(s.files.has('acc-reset.bin'), 'no partial file').toBe(false);
      expect([...s.files.keys()].sort(), 'the other files are as before').toEqual([...before.keys()].sort());
      const again = await connect(dk);
      const ok = pattern(MIB + 100, 12);
      expect(await again.session.put(SAMPLES, rp('acc-reset.bin'), ok, { expected: hashU32(ok, dt2) }), 'the next upload after the reset').toBe(hashU32(ok, dt2));
      await rig.session.expectNoFault('usb-fast staging buffer');
      // eslint-disable-next-line no-console
      console.log(`ACC buffer sizes obtained (bytes): ${JSON.stringify(obtained)}`);
    }, 3_600_000);

    test('storage bench: STATUS "B" runs every test, "R" returns the times, and no bench file is left', async () => {
      const { dk } = await boot();
      const before = (await state(dk, 'bn0')).files;
      const b = await dk.request(0x37, Uint8Array.from([0x42, 0x3f, 1])); // 'B', all tests, 1 MiB
      if (b.status !== 0 || b.body.length !== 4) {
        console.log('storage bench skipped: the module is built without UF_BENCH (STATUS answers E_MSG for the tag)');
        return;
      }
      expect(Buffer.from(b.body).readUInt32BE(0), 'started').toBe(1);
      let r = Buffer.alloc(0);
      for (let i = 0; i < 400; i++) {
        await rig.runtime.ui.wait({ ms: 500, why: SETTLE });
        r = Buffer.from((await dk.request(0x37, Uint8Array.from([0x52]))).body); // 'R'
        if (r.readUInt32BE(4) === 10 && r.readUInt32BE(0) === 0) break;
      }
      expect([r.readUInt32BE(0), r.readUInt32BE(4)], 'the run is over').toEqual([0, 10]);
      const ms = Array.from({ length: 14 }, (_, i) => r.readUInt32BE(8 + 4 * i));
      // slots: s64 s1m d64 d256 d1m dcl dcl.cls dcl.del r64 r1m h32 scl scl.cls scl.del
      for (const slot of [0, 1, 8, 9, 10, 11, 12, 13]) expect(ms[slot], `slot ${slot} has a time`).toBeGreaterThan(0);
      if (dt2) for (const slot of [2, 3, 4, 5, 6, 7]) expect(ms[slot], `+Drive slot ${slot} has a time`).toBeGreaterThan(0);
      const after = await state(dk, 'bn1');
      unchanged(before, after.files, []);
      expect(after.files.has('dkbench.bin'), 'no store bench file').toBe(false);
      await rig.session.expectNoFault('usb-fast storage bench');
    }, 1_800_000);

    test.each(MODES)('$label: the runtime expires the session when the host is gone: OUT-only frees the bulk session, the pair answers E_STATE at the next write and frees the staging buffer', async (mode) => {
      const { dk, bulk } = await boot(mode);
      const link = bulk.link;
      const status = async () => {
        const b = Buffer.from((await dk.request(0x37)).body);
        return { cap: b.readUInt32BE(64), expired: b.readUInt32BE(68) };
      };
      const f = pattern(3 << 20, 9);
      await link.send(req(BULK_CMD.BEGIN, 1, beginBody('expire.bin', f.length, be32(hashU32(f, dt2)))));
      expect((await replies(link, 1))[0]!.status).toBe(0);
      await link.send(dataFrame(2, 0, f.subarray(0, 65_536)));
      expect((await replies(link, 1))[0]!.status).toBe(0);
      expect((await status()).cap, 'a staging buffer is held').toBeGreaterThan(0);
      for (let t = 0; t < 23_000; t += 1000) await rig.runtime.ui.wait({ ms: 1000, why: SETTLE });
      if (mode.outOnly) expect(await status(), 'freed, one session dropped').toEqual({ cap: 0, expired: 1 });
      if (mode.outOnly) {
        // OUT-only: the module also frees the receive ring at expiry, so a DATA frame is NAKed and nothing is queued for the 'R' poll; the next nonce starts a session again
        const b = Buffer.from((await dk.request(0x37)).body);
        expect(b.readUInt32BE(32) & 0x10, 'the receive ring is freed (DK_USB_BULK_LIVE clear)').toBe(0);
        expect((await dk.usbFastReplies()).length, 'the R poll has nothing').toBe(0);
        const again = await connect(dk, { mode });
        expect((await again.session.info()).flags & 1, 'a new session: INFO answers over SysEx').toBe(1);
      } else {
        // the DATA is buffered (accepted); the tail flush of END reaches the runtime, which answers E_STATE: the file is dropped and the buffer freed
        await link.send(dataFrame(3, 65_536, f.subarray(65_536, 131_072)));
        expect((await replies(link, 1))[0]!.status, 'the DATA is accepted').toBe(0);
        await link.send(req(BULK_CMD.END, 4));
        expect((await replies(link, 1))[0]!.status, 'the host sees the session gone').toBe(BULK_STATUS.E_STATE);
        expect((await status()).cap, 'the staging buffer is freed').toBe(0);
      }
      await rig.session.expectNoFault('usb-fast session expiry');
    }, 1_800_000);

    test('pair: the session expires while a half is pending and a DATA frame is half received (copy/write overlap): no stray write, the frame gets E_STATE, the file is not verified, the next upload works', async () => {
      const { dk, bulk } = await boot(PAIR);
      const link = bulk.link;
      const half = 512 * 1024;
      const f = pattern(3 << 20, 12);
      const before = (await state(dk, 'ue0')).files;
      await link.send(req(BULK_CMD.BEGIN, 1, beginBody('expire-half.bin', f.length, be32(hashU32(f, dt2)))));
      expect((await replies(link, 1))[0]!.status).toBe(0);
      // seven frames of 64 KiB: the first half is nearly full and nothing was written yet
      for (let i = 0; i < 7; i++) {
        await link.send(dataFrame(2 + i, i * 65_536, f.subarray(i * 65_536, (i + 1) * 65_536)));
        expect((await replies(link, 1))[0]!.status, `DATA ${i + 1} is accepted`).toBe(0);
      }
      for (let t = 0; t < 23_000; t += 1000) await rig.runtime.ui.wait({ ms: 1000, why: SETTLE }); // the runtime ends the session: the next write answers E_STATE
      // the frame that fills the half (its write fails with E_STATE) and the head of the next frame, which stays half received at that moment
      const next = dataFrame(10, half, f.subarray(half, half + 65_536));
      const head = 40_000;
      await link.send(Buffer.concat([dataFrame(9, half - 65_536, f.subarray(half - 65_536, half)), next.subarray(0, head)]));
      const got = await replies(link, 2);
      expect(got[0]!.status, 'the frame that filled the half is accepted').toBe(0);
      expect(got[1], 'the half received frame is answered with E_STATE').toMatchObject({ seq: 10, status: BULK_STATUS.E_STATE });
      // the rest of that frame is skipped, not copied anywhere
      await link.send(next.subarray(head));
      for (let i = 0; i < 20; i++) await rig.runtime.ui.wait({ ms: 50, why: SETTLE });
      await link.send(req(BULK_CMD.END, 11));
      const end = await replies(link, 1);
      expect(end[0]!.status, 'END does not verify a file that is gone').not.toBe(0);
      expect((await state(dk, 'ue1')).files.has('expire-half.bin'), 'the file is not on the card').toBe(false);
      // a new link and a new file: the module is healthy
      const again = await connect(dk, { mode: PAIR });
      const ok = pattern(900_001, 13);
      await again.session.put(SAMPLES, rp('after-expire.bin'), ok, { expected: hashU32(ok, dt2) });
      const s = await state(dk, 'ue2');
      await expectFile(dk, s, 'after-expire.bin', ok, 'after the expiry');
      unchanged(before, s.files, ['after-expire.bin']);
      await rig.session.expectNoFault('usb-fast expiry under a half received frame');
    }, 1_800_000);

    test.each(MODES)('$label: the receive ring: INFO reports the window and the host keeps that many DATA frames in flight, a bus reset with a full ring commits nothing, the switch off or a host that never asked INFO keeps 2', async (mode) => {
      const { dk, bulk } = await boot(mode);
      /** STATUS 'S' 1 [value]: the ring switch of core/usb-bulk, bit 1 of the mask (reads without a value). */
      const ring = async (value?: number) => {
        const r = await dk.request(0x37, Uint8Array.from(value === undefined ? [0x53] : [0x53, 1, value]));
        expect(r.status, 'STATUS S').toBe(0);
        return { on: (Buffer.from(r.body).readUInt32BE(0) >>> 1) & 1 };
      };
      /** The most DATA requests sent and not yet answered, seen at the link. */
      const meter = (link: BulkLink) => {
        const m = { now: 0, max: 0 };
        const send = link.send.bind(link);
        const recv = link.recv.bind(link);
        link.send = async (f: Uint8Array) => {
          await send(f);
          if (f[4] === BULK_CMD.DATA) m.max = Math.max(m.max, ++m.now);
        };
        link.recv = async (t: number, sig?: AbortSignal) => {
          const r = await recv(t, sig);
          if (r && r[4] === (BULK_CMD.DATA | 0x40)) m.now--;
          return r;
        };
        return m;
      };
      const before = (await state(dk, 'ur0')).files;
      expect(await ring(), 'the ring is on by default: 60 slots, 12 frames of 64 KiB').toEqual({ on: 1 });
      expect((await bulk.session.info()).window, 'INFO carries the window').toBe(12);
      const m1 = meter(bulk.link);
      const big = pattern(3 << 20, 21);
      expect(await bulk.session.put(SAMPLES, rp('ring.bin'), big, { expected: hashU32(big, dt2) }), 'a file up with the window').toBe(hashU32(big, dt2));
      expect(m1.max, 'twelve DATA frames in flight').toBe(12);
      let s = await state(dk, 'ur1');
      await expectFile(dk, s, 'ring.bin', big, 'ring.bin');

      // a host that never asked INFO (an old host) keeps 2, whatever the device offers
      const old = new BulkSession(bulk.link);
      const m2 = meter(bulk.link);
      const small = pattern(1 << 20, 22);
      await old.put(SAMPLES, rp('old-host.bin'), small, { expected: hashU32(small, dt2) });
      expect(m2.max, 'no INFO read: two in flight').toBe(2);

      // a bus reset while the ring is full: nothing is committed, the next upload works on a new link
      const reset = await connect(dk, { timeoutMs: 1500, mode });
      const connectAddr = await mode.callback(call);
      let cut = false;
      const failed = await reset.session
        .put(SAMPLES, rp('ring-reset.bin'), pattern(4 << 20, 23), {
          onProgress: (sent) => {
            if (!cut && sent >= 1_500_000) {
              cut = true;
              void rpc(rig.session, 'usb.bulk.in', { connect: connectAddr });
            }
          },
        })
        .catch((e) => e);
      expect(cut, 'the reset was delivered mid-file').toBe(true);
      expect(failed, 'the upload fails').toBeInstanceOf(Error);
      s = await state(dk, 'ur2');
      expect(s.files.has('ring-reset.bin'), 'no partial file').toBe(false);
      const again = await connect(dk, { mode });
      const ok = pattern(300_000, 24);
      await again.session.put(SAMPLES, rp('ring-reset.bin'), ok, { expected: hashU32(ok, dt2) });
      s = await state(dk, 'ur3');
      await expectFile(dk, s, 'ring-reset.bin', ok, 'after the reset with a full ring');

      // the switch off: the old 8 slots, the host keeps 2; on again: 12
      expect(await ring(0), 'switch off').toEqual({ on: 0 });
      const off = await connect(dk, { mode });
      expect((await off.session.info()).window, 'INFO: no window').toBe(0);
      const m3 = meter(off.link);
      await off.session.put(SAMPLES, rp('ring-off.bin'), small, { expected: hashU32(small, dt2) });
      expect(m3.max, 'switch off: two in flight').toBe(2);
      expect(await ring(1), 'switch on').toEqual({ on: 1 });
      const on = await connect(dk, { mode });
      const m4 = meter(on.link);
      await on.session.put(SAMPLES, rp('ring-on.bin'), small, { expected: hashU32(small, dt2) });
      expect(m4.max, 'switch on: twelve in flight').toBe(12);
      s = await state(dk, 'ur4');
      for (const n of ['old-host.bin', 'ring-off.bin', 'ring-on.bin']) await expectFile(dk, s, n, small, n);
      unchanged(before, s.files, ['ring.bin', 'old-host.bin', 'ring-reset.bin', 'ring-off.bin', 'ring-on.bin']);
      await rig.session.expectNoFault('usb-fast receive ring');
    }, 3_600_000);

    test('a device in USB AUDIO/MIDI holds no bulk session until a nonce; the switch to USB MIDI brings the bulk pair up on a timer of the module', async () => {
      // The harness boots in USB MIDI, so the boot's enable happens first (the job's first step runs at init); the switch to port 5 then ends that session.
      const dk = await rig.pausedBoot(await baseCard());
      await rig.runtime.ui.wait({ ms: 2500, why: SETTLE });
      await setPort(5);
      await rig.runtime.ui.wait({ ms: 3000, why: SETTLE });
      const status = async () => {
        const b = Buffer.from((await dk.request(0x37)).body);
        return { enabled: (b.readUInt32BE(28) & 4) !== 0, ring: b.readUInt32BE(76) };
      };
      expect(await status(), 'USB AUDIO/MIDI without a nonce: no session, no ring').toEqual({ enabled: false, ring: 0 });
      await setPort(2);
      await rig.runtime.ui.wait({ ms: 1500, why: SETTLE });
      expect((await status()).enabled, 'USB MIDI again: the job enabled the bulk session').toBe(true);
      const bulk = await connect(dk, { mode: PAIR });
      expect((await bulk.session.info()).flags & 1, 'the pair answers INFO on its own endpoint').toBe(0);
      await rig.session.expectNoFault('usb-fast mode switch');
    }, 1_800_000);

    test('bench: MB/s in emulated time, bulk against SysEx', async () => {
      const { dk } = await boot();
      const size = 1 << 20;
      const data = pattern(size, 55);
      const fast = await connect(dk, { stepInstructions: 20_000 });
      const clock = async (run: () => Promise<unknown>) => {
        const a = await elapsed();
        await run();
        return ((await elapsed()) - a) / 132e6; // seconds of emulated time at 132 MHz
      };
      const lines: string[] = [];
      const tBulk = await clock(() => fast.session.put(STORE, 'data/bench-bulk.bin', data, { expected: hashU32(data, false) }));
      lines.push(`bulk to the store: ${(size / 1e6 / tBulk).toFixed(2)} MB/s`);
      // the same upload with the receive ring off (the old 8 slots and window 2): a sanity check only, the emulator is not a speed reference
      await dk.request(0x37, Uint8Array.from([0x53, 1, 0]));
      const old = await connect(dk, { stepInstructions: 20_000 });
      const tOld = await clock(() => old.session.put(STORE, 'data/bench-bulk-old.bin', data, { expected: hashU32(data, false) }));
      lines.push(`bulk to the store, ring off: ${(size / 1e6 / tOld).toFixed(2)} MB/s`);
      await dk.request(0x37, Uint8Array.from([0x53, 1, 1]));
      const tSys = await clock(() => dk.put('data/bench-sysex.bin', data));
      lines.push(`SysEx to the store: ${(size / 1e6 / tSys).toFixed(2)} MB/s`);
      if (dt2) {
        const tDrive = await clock(() => fast.session.put(SAMPLES, rp('bench.bin'), data, { expected: hashU32(data, true) }));
        lines.push(`bulk to the +Drive: ${(size / 1e6 / tDrive).toFixed(2)} MB/s`);
      }
      // eslint-disable-next-line no-console
      console.log(`BENCH ${device.toUpperCase()} 1 MiB, emulated time (132 MHz model): ${lines.join('; ')}`);
      expect(tBulk, 'bulk is faster than SysEx in the model').toBeLessThan(tSys);
    }, 3_600_000);
  });
}
