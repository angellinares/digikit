import { execFile } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { promisify } from 'node:util';
import { BULK_CMD, BULK_STATUS } from '@digi/dk-session/gen_bulk';
import {
  BulkDeviceError,
  EmuBulkLink,
  bulkRequest,
  chooseBulk,
  fileHash,
  parseBulkReply,
  type BulkChoice,
  type DkSession,
} from '@digi/dk-session';
import { repoRoot } from '@digi/test-support';
import { describe, expect, test } from 'vitest';
import { overlayArgs, plusdriveBinary, pokeKnob } from '@digi/regression/runtime/card-kit.ts';
import { cardRig, RT, type CardRig } from '@digi/regression/runtime/card-rig.ts';
import { DEVICES, FIRMWARE } from '@digi/regression/runtime/devices.ts';
import { planSymbol } from '@digi/regression/runtime/runtime-plan.ts';
import { advance, rpc, word } from '@digi/regression/runtime/fx-live.ts';

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
      roots: [...RT, 'core/usb-bulk', ...(dt2 ? ['core/plusdrive-write'] : [])],
      symbols: ['DKRT_MAILBOX', 'DKRT_SESSION_IDLE', 'DKRT_UPLOAD_CUT', 'DKRT_UPLOAD_TEAR', 'DKRT_UPLOAD_WRITES', ...(dt2 ? ['DKRT_PD_CUT'] : [])],
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
      options: { stepInstructions?: number; zlp?: boolean; timeoutMs?: number; chunk?: number } = {},
    ): Promise<BulkChoice> => {
      if (dt2 && !recovered.has(dk)) {
        recovered.add(dk);
        await advance(rig.session, 2000); // the boot recovery of the staging folder
      }
      for (let i = 0; i < 100 && (await EmuBulkLink.connectCallback(call)) === undefined; i++) await advance(rig.session, 100);
      const notes: string[] = [];
      const opener = EmuBulkLink.opener(call, { stepInstructions: options.stepInstructions ?? STEP, zlp: options.zlp });
      const choice = await chooseBulk(dk, opener, (l) => notes.push(l), { timeoutMs: options.timeoutMs ?? 20_000, resyncMs: 200, chunk: options.chunk, log: (l) => notes.push(l) });
      const dbg = choice ? '' : JSON.stringify(await rpc(rig.session, 'usb.bulk.out', {}).then((r) => (r as { status: unknown }).status)) + ' module ' + Buffer.from((await dk.request(0x37)).body).toString('hex');
      expect(choice, `${dbg} a bulk link is negotiated (${JSON.stringify(options)}, connect ${++connects}: ${notes.join('; ')})`).toBeDefined();
      return choice!;
    };
    const boot = async (name = 'uf-base') => {
      const dk = await rig.pausedBoot(await baseCard(name));
      return { dk, bulk: await connect(dk) };
    };
    const elapsed = async () => Number((await rig.session.status()).elapsed);

    test('uploads reach the DN2 store or the DT2 +Drive with their hash; INFO carries the nonce; VERIFY is reserved; REPLACE and E_EXISTS', async () => {
      const { dk, bulk } = await boot();
      const before = (await state(dk, 'uf0')).files;
      expect(before.size, 'the seed files are there').toBeGreaterThanOrEqual(2);
      const info = await bulk.session.info();
      expect([info.version, info.dataMax, info.align, info.nonce.length]).toEqual([1, 65536, 512, 8]);

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

    test('ABORT drops the open file and a cancel mid-transfer leaves the old file; a USB reset in the middle of a file leaves nothing and the next upload works', async () => {
      const { dk, bulk } = await boot('uf-base');
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
      const reset = await connect(dk, { timeoutMs: 1500 });
      const big = pattern(600_000, 6);
      const connectAddr = await EmuBulkLink.connectCallback(call);
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
      const again = await connect(dk);
      await again.session.put(SAMPLES, rp('reset.bin'), ok, { expected: hashU32(ok, dt2) });
      s = await state(dk, 'ua4');
      await expectFile(dk, s, 'reset.bin', ok, 'after the reset');
      await rig.session.expectNoFault('usb-fast abort and reset');
    }, 1_800_000);

    test('frames that span receive buffers and buffers with many frames, around 16 KiB and the 512-byte packet, with and without the zero-length packet', async () => {
      const { dk } = await boot();
      const before = (await state(dk, 'uz0')).files;
      // chunk sizes: 512 (528-byte frames, about 31 to a 16 KiB buffer), 16 KiB (a frame is 16 bytes over a buffer), 65536 (a frame spans five buffers). File sizes: the last
      // DATA frame is 12 + 4 + n bytes: n = 496 and 16368 make a frame of exactly 512 and 16384 bytes (the zero-length packet ends it; 16384 fills a buffer
      // and the packet gets a dTD of its own), 16369 and 16385 are one byte over.
      const sizes = [1, 496, 497, 511, 512, 1000, 16_368, 16_369, 16_384, 16_385, 16_368 + 512 * 3, 65_536, 65_536 + 496, 100_000];
      let k = 0;
      for (const chunk of [512, 16_384, 65_536]) {
        const { session } = await connect(dk, { chunk });
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
      const raw = (await connect(dk, { zlp: false })).link;
      const rq = (cmd: number, seq: number, body: Buffer) => raw.send(bulkRequest(cmd, seq, body));
      const be32 = (n: number) => Buffer.from([n >>> 24, (n >>> 16) & 255, (n >>> 8) & 255, n & 255]);
      const data = pattern(496, 77);
      await rq(BULK_CMD.BEGIN, 1, Buffer.concat([be32(SAMPLES), Buffer.from(`${rp('nozlp.bin')}\0`), be32(data.length), Buffer.alloc(4)]));
      expect(parseBulkReply((await raw.recv(20_000))!).status, 'BEGIN').toBe(0);
      await rq(BULK_CMD.DATA, 2, Buffer.concat([be32(0), data])); // the frame is 512 bytes: no packet follows
      for (let i = 0; i < 40; i++) await advance(rig.session, 50);
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
      for (let i = 0; i < 20; i++) await advance(rig.session, 50);
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
        if (over.status === BULK_STATUS.L_E_BUSY) await advance(rig.session, 20);
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
      await advance(rig.session, 1);
      await rpc(rig.session, 'usb.bulk.in', { connect: (await EmuBulkLink.connectCallback(call))! });
      await advance(rig.session, 200);
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
        else expect(s.files.has(target), `${label}: nothing at the destination`).toBe(false);
        unchanged(before, s.files, [target, 'Cut/cut.bin']);
        // the next upload works, and the destination is as before it
        const more = pattern(9000, 41);
        await bulk2.session.put(SAMPLES, rp('after.bin'), more, { expected: hashU32(more, dt2) });
        const t = await state(next, `pc${k}d`);
        await expectFile(next, t, 'after.bin', more, `${label}: the next upload`);
        expect(t.files.get(target), `${label}: the destination is unchanged by it`).toEqual(s.files.get(target));
      }
    }, 3_600_000);

    test('storage bench: STATUS "B" runs every test, "R" returns the times, and no bench file is left', async () => {
      const { dk } = await boot();
      const before = (await state(dk, 'bn0')).files;
      const b = await dk.request(0x37, Uint8Array.from([0x42, 0x3f, 1])); // 'B', all tests, 1 MiB
      if (b.body.length !== 4) {
        console.log('storage bench skipped: the module is built without UF_BENCH (STATUS answers the 64-byte status)');
        return;
      }
      expect(Buffer.from(b.body).readUInt32BE(0), 'started').toBe(1);
      let r = Buffer.alloc(0);
      for (let i = 0; i < 400; i++) {
        await advance(rig.session, 500);
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
