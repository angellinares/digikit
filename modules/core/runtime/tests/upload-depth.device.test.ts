import fs from 'node:fs';
import path from 'node:path';
import { randomBytes } from 'node:crypto';
import { DkError, DkSession, EmuUsbTransport, fileHashHex, parseReply, type DkTransport } from '@digi/dk-session';
import { expect } from 'vitest';
import { liveCheck as check } from '@digi/regression/runtime/artifacts.ts';
import { uploadTests } from '@digi/regression/runtime/upload-kit.ts';

/**
 * `DkSession.put` with two DATA requests in flight (finding 43, "Two requests in flight"), against the runtime on the emulated USB, on the digikit
 * store of both devices: the bytes and the hash of every size, a flipped bit in one chunk (END answers E_HASH, nothing is committed, the session goes on),
 * and a lost DATA reply (a timeout, the unacknowledged chunks are sent again from the first).
 */

const DATA = 0x11;
const CHUNK = 32768;

/** The emulator's USB with a hand on it: counts the DATA requests that wait for a reply, can flip a payload bit of one request, and can lose one reply. */
class Link implements DkTransport {
  readonly dataDepth = 2;
  outstanding = 0;
  maxOutstanding = 0;
  dataSent = 0;
  flipAtRequest = -1;
  dropReplyOf = -1;
  private readonly seqs: number[] = [];
  private dropSeq = -1;
  constructor(private readonly base: EmuUsbTransport) {}
  async send(msg: Uint8Array): Promise<void> {
    if (msg[5] === DATA) {
      const n = this.dataSent++;
      this.outstanding++;
      this.maxOutstanding = Math.max(this.maxOutstanding, this.outstanding);
      if (n === this.flipAtRequest) {
        msg = Uint8Array.from(msg);
        msg[7 + 8 * 20 + 3] ^= 1; // a data byte of the 8-in-7 packing (not a collector byte), well inside the chunk
      }
      if (n === this.dropReplyOf) this.dropSeq = msg[6]!;
      this.seqs.push(msg[6]!);
    }
    return this.base.send(msg);
  }
  async recv(timeoutMs: number, signal?: AbortSignal): Promise<Uint8Array | null> {
    for (;;) {
      const raw = await this.base.recv(timeoutMs, signal);
      const r = raw ? parseReply(raw) : null;
      if (r && r.cmd === DATA && this.seqs.includes(r.seq)) {
        this.outstanding--;
        if (r.seq === this.dropSeq) {
          this.dropSeq = -1;
          continue; // lost on the way
        }
      }
      return raw;
    }
  }
}

uploadTests('upload-depth: two DATA requests in flight on the digikit store', async (rig) => {
  await rig.pausedBoot(await rig.card({ name: 'depth' }));
  await rig.applyCap();
  const call = (method: string, params?: Record<string, unknown>) => rig.session.client.call(method, params);
  const open = (timeoutMs: number) => {
    const link = new Link(new EmuUsbTransport(call as never, { stepInstructions: 400_000 }));
    return { link, dk: new DkSession(link, { timeoutMs }) };
  };
  const { link, dk } = open(60_000);
  const sent: Record<string, Buffer> = {};
  const put = async (name: string, data: Buffer, session: DkSession = dk) => {
    await session.put(name, data);
    sent[name] = data;
  };

  // sizes: a chunk edge, under two chunks, a partial last chunk, and several windows of 8 chunks (the window of the session is 8 chunks)
  for (const n of [1, 512, CHUNK, CHUNK + 1, 2 * CHUNK + 700, 9 * CHUNK + 3]) {
    const data = randomBytes(n);
    await put(`depth/f${n}`, data);
    check(`${n} bytes: VERIFY equals the host's hash`, (await dk.verify(`depth/f${n}`)) === fileHashHex(data));
  }
  check('two DATA requests were in flight', link.maxOutstanding === 2, String(link.maxOutstanding));
  check('every request was answered', link.outstanding === 0, String(link.outstanding));
  await put('depth/empty', Buffer.alloc(0));
  const listed = Object.fromEntries((await dk.list()).map((e) => [e.path, e.size]));
  expect(listed, 'LIST holds the files sent').toEqual(Object.fromEntries(Object.entries(sent).map(([p, d]) => [p, d.length])));
  const view = await rig.view('depth-ok');
  expect(
    Object.fromEntries(Object.entries(view.files).map(([p, d]) => [p, fileHashHex(d)])),
    `the card, read by plusdrive, holds the bytes sent ${view.head}`,
  ).toEqual(Object.fromEntries(Object.entries(sent).map(([p, d]) => [p, fileHashHex(d)])));

  // a bit flipped in the 5th DATA request: END compares the hash of what arrived and refuses; nothing is committed
  const bad = randomBytes(6 * CHUNK + 11);
  link.flipAtRequest = link.dataSent + 4;
  const err = await dk.put('depth/bad', bad).then(
    () => null,
    (e: unknown) => e,
  );
  check(
    'a flipped bit: the upload ends with E_HASH',
    err instanceof DkError && err.status === 'E_HASH',
    String(err instanceof Error ? err.message : err),
  );
  check('a flipped bit: nothing was committed', !(await dk.list()).some((e) => e.path === 'depth/bad'));
  link.flipAtRequest = -1;
  await put('depth/bad', bad); // the same path and bytes again, whole
  check('the retry of the file is stored whole', (await dk.verify('depth/bad')) === fileHashHex(bad));

  // a lost reply of the 3rd DATA request of the next file, one more request behind it: a later ok reply shows it was done
  const lost = open(15_000);
  const a = randomBytes(5 * CHUNK + 5);
  lost.link.dropReplyOf = 2;
  await put('depth/lost-mid', a, lost.dk);
  check('a lost reply (a request behind it): the file is whole', (await dk.verify('depth/lost-mid')) === fileHashHex(a));
  check('a lost reply (a request behind it): no request was sent again', lost.link.dataSent === 6, String(lost.link.dataSent));
  // a lost reply of the last request of the file: a timeout, the exact repeat of the last chunk is taken
  const b = randomBytes(3 * CHUNK + 9);
  const last = open(15_000);
  last.link.dropReplyOf = 3;
  await put('depth/lost-last', b, last.dk);
  check('a lost reply (the last chunk): the file is whole', (await dk.verify('depth/lost-last')) === fileHashHex(b));
  check('a lost reply (the last chunk): the chunk was sent again', last.link.dataSent > 4, String(last.link.dataSent));

  fs.writeFileSync(path.join(rig.artifacts, 'depth.json'), JSON.stringify({ files: Object.keys(sent) }));
  const final = await rig.view('depth-final');
  expect(
    Object.fromEntries(Object.entries(final.files).map(([p, d]) => [p, fileHashHex(d)])),
    'the card holds every file sent and no other',
  ).toEqual(Object.fromEntries(Object.entries(sent).map(([p, d]) => [p, fileHashHex(d)])));
});
