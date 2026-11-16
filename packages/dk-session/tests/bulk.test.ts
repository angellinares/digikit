// The bulk link's host side against the fake device: frames, the session (two DATA in flight, E_BUSY and its budget, hash checks, REPLACE, cancel, late
// replies, resync, ABORT paths, ZLP, a corrupt stream), the nonce handshake, and the samples backend that sends file bytes over bulk, falls back to SysEx
// when the link fails and keeps everything else on the SysEx or MidiRpc backend.
import assert from 'node:assert/strict';
import { describe, expect, test } from 'vitest';
import {
  BulkDeviceError,
  BulkFrames,
  BulkSession,
  bulkReply,
  bulkRequest,
  chooseBulk,
  fileHash,
  parseBulkReply,
  ProcessBulkLink,
  type BulkLink,
} from '../src/index.ts';
import type { Prepared } from '../src/dksample.ts';
import { FakeBulkDevice, hash4 } from '../src/bulk-fake.ts';
import { BULK_CMD, BULK_FLAG, BULK_STATUS } from '../src/gen_bulk.ts';
import { openSamples } from '../src/samples.ts';
import { TRAILER } from '../src/rpcfiles.ts';

const bytes = (n: number) => Uint8Array.from({ length: n }, (_, i) => (i * 131 + 7) & 255);
const hash = hash4;
const session = (dev: BulkLink, o: object = {}) =>
  new BulkSession(dev, { chunk: 4096, timeoutMs: 20, resyncMs: 5, busyWaitMs: 0, ...o });
const timeoutOf = (p: Promise<unknown>) =>
  p.then(
    () => null,
    (e) => e,
  );

describe('frames', () => {
  test('the header is the one of the plan', () => {
    const r = bulkRequest(BULK_CMD.DATA, 7, Uint8Array.of(1, 2, 3));
    expect(Buffer.from(r.subarray(0, 12)).toString('hex')).toBe('444b4231030700000000' + '0003');
    const p = parseBulkReply(bulkReply(BULK_CMD.END, 9, -56, Uint8Array.of(0xaa)));
    expect([p.cmd, p.seq, p.status, [...p.body]]).toEqual([BULK_CMD.END, 9, -56, [0xaa]]);
  });
  test('chunks may split or join frames, empty chunks add nothing; a corrupt stream throws', () => {
    const a = bulkReply(1, 1, 0, Uint8Array.of(1, 2, 3));
    const b = bulkReply(2, 2, 0);
    const all = Uint8Array.from([...a, ...b]);
    const f = new BulkFrames();
    const got = [
      ...f.feed(all.subarray(0, 5)),
      ...f.feed(new Uint8Array()),
      ...f.feed(all.subarray(5, 14)),
      ...f.feed(all.subarray(14)),
    ];
    expect(got.map((x) => [...x])).toEqual([[...a], [...b]]);
    expect(() => new BulkFrames().feed(new Uint8Array(12))).toThrow(/magic/);
    const huge = bulkReply(1, 1, 0);
    new DataView(huge.buffer).setUint32(8, 0xffffffff);
    expect(() => new BulkFrames().feed(huge)).toThrow(/over the bound/);
  });
  test('the hash of the fake device is the odd content hash the Rust side computes', () => {
    expect(hash(new TextEncoder().encode('abc'))).toBe(0x49bec845);
  });
});

describe('BulkSession batch commit (DEFER, the 8-byte END and COMMIT)', () => {
  test('DEFER is sent in BEGIN, put returns the hash and the files pending, COMMIT names them', async () => {
    const dev = new FakeBulkDevice();
    dev.batch = true;
    const s = session(dev);
    const a = bytes(3000);
    const b = bytes(5000);
    const ra = await s.put(1, 'a', a, { expected: hash(a), defer: true });
    expect(ra).toEqual({ hash: hash(a), pending: 1 });
    expect(dev.files.has('a')).toBe(false); // verified, not named yet
    const rb = await s.put(1, 'b', b, { defer: true, replace: true });
    expect(rb).toEqual({ hash: hash(b), pending: 2 });
    const begins = dev.log.filter(([c]) => c === BULK_CMD.BEGIN).map(([, f]) => f);
    expect(begins).toEqual([BULK_FLAG.DEFER, BULK_FLAG.DEFER | BULK_FLAG.REPLACE]);
    expect(await s.commit()).toBe(0);
    expect([...dev.files.keys()]).toEqual(['a', 'b']);
    expect(dev.pending.size).toBe(0);
  });

  test('a limit of the device commits at END: the reply says pending 0', async () => {
    const dev = new FakeBulkDevice();
    dev.batch = true;
    dev.pendingMax = 2;
    const s = session(dev);
    expect((await s.put(1, 'a', bytes(512), { defer: true })).pending).toBe(1);
    expect((await s.put(1, 'b', bytes(512), { defer: true })).pending).toBe(0);
    expect(dev.files.size).toBe(2);
  });

  test('without defer the flag stays clear and put returns the hash alone', async () => {
    const dev = new FakeBulkDevice();
    dev.batch = true;
    const data = bytes(512);
    expect(await session(dev).put(1, 'a', data)).toBe(hash(data));
    expect(dev.log.find(([c]) => c === BULK_CMD.BEGIN)![1]).toBe(0);
    expect(dev.files.has('a')).toBe(true);
  });

  test('an old device: the 4-byte END reads as committed (pending undefined), COMMIT is E_PROTO and reads as nothing to commit', async () => {
    const dev = new FakeBulkDevice(); // batch off: DEFER ignored
    const s = session(dev);
    const data = bytes(1024);
    const r = await s.put(1, 'a', data, { defer: true });
    expect(r).toEqual({ hash: hash(data), pending: undefined });
    expect(dev.files.has('a')).toBe(true);
    expect(await s.commit()).toBe(0);
    expect(dev.log.at(-1)![0]).toBe(BULK_CMD.COMMIT);
  });

  test('a COMMIT error is thrown with its status and the files stay pending on the device', async () => {
    const dev = new FakeBulkDevice();
    dev.batch = true;
    const s = session(dev, { busyRetries: 0 });
    await s.put(1, 'a', bytes(512), { defer: true });
    dev.commitError = BULK_STATUS.E_WRITE;
    await expect(s.commit()).rejects.toMatchObject({ code: BULK_STATUS.E_WRITE });
    expect(dev.pending.size).toBe(1);
    expect(await s.commit()).toBe(0); // the next one works
    expect(dev.files.has('a')).toBe(true);
  });
});

describe('BulkSession.put', () => {
  test('two DATA requests in flight, in order, and the device hash comes back', async () => {
    const dev = new FakeBulkDevice();
    const data = bytes(10 * 4096 + 100);
    const seen: [number, number][] = [];
    const h = await session(dev).put(1, 'a/b.bin', data, {
      expected: hash(data),
      onProgress: (a, t) => seen.push([a, t]),
    });
    expect(h).toBe(hash(data));
    expect(dev.files.get('a/b.bin')).toEqual(data);
    expect(dev.maxInflight).toBe(2);
    expect(dev.applied).toEqual(Array.from({ length: 11 }, (_, i) => i * 4096));
    expect(seen[0]).toEqual([0, data.length]);
    expect(seen.at(-1)).toEqual([data.length, data.length]);
  });

  test('DATA requests in flight: the window INFO reported, an explicit window wins, a device that reports none keeps 2', async () => {
    const data = bytes(24 * 4096);
    const reported = new FakeBulkDevice();
    reported.infoWindow = 6;
    const s = session(reported);
    expect((await s.info()).window).toBe(6);
    await s.put(1, 'f', data, { expected: hash(data) });
    expect(reported.maxInflight).toBe(6);
    expect(reported.files.get('f')).toEqual(data);
    const explicit = new FakeBulkDevice();
    explicit.infoWindow = 6;
    const e = session(explicit, { window: 3 });
    await e.info();
    await e.put(1, 'f', data);
    expect(explicit.maxInflight).toBe(3);
    const old = new FakeBulkDevice(); // an old device: INFO byte 2 is 0
    const o = session(old);
    expect((await o.info()).window).toBe(0);
    await o.put(1, 'f', data);
    expect(old.maxInflight).toBe(2);
    const noinfo = new FakeBulkDevice(); // INFO never asked: 2
    noinfo.infoWindow = 6;
    await session(noinfo).put(1, 'f', data);
    expect(noinfo.maxInflight).toBe(2);
  });

  test('E_BUSY on DATA: the next request goes out at the first byte not acknowledged', async () => {
    const dev = new FakeBulkDevice();
    dev.busy.set(BULK_CMD.DATA, 3);
    const data = bytes(12 * 4096);
    await session(dev).put(1, 'f', data, { expected: hash(data) });
    expect(dev.files.get('f')).toEqual(data);
    expect(dev.applied).toEqual(Array.from({ length: 12 }, (_, i) => i * 4096));
  });

  test('the busy budget is reset by every acknowledged DATA; a stuck device gives up and leaves nothing', async () => {
    const dev = new FakeBulkDevice();
    dev.busyBursts = [2, 5]; // 10 busy answers in all, never more than 2 in a row
    await session(dev, { busyRetries: 3 }).put(1, 'f', bytes(6 * 4096));
    expect(dev.files.has('f')).toBe(true);
    const stuck = new FakeBulkDevice();
    stuck.busy.set(BULK_CMD.DATA, 1000);
    await expect(session(stuck, { busyRetries: 5 }).put(1, 'g', bytes(1000))).rejects.toMatchObject({
      status: 'L_E_BUSY',
    });
    expect(stuck.sessionOpen).toBe(false);
  });

  test('BEGIN that stays busy gets one ABORT and one more try', async () => {
    const dev = new FakeBulkDevice();
    dev.mute.add(BULK_CMD.DATA);
    dev.mute.add(BULK_CMD.ABORT);
    const lines: string[] = [];
    const s = session(dev, { busyRetries: 2, log: (l: string) => lines.push(l) });
    expect((await timeoutOf(s.put(1, 'a', bytes(8192)))).kind).toBe('timeout');
    expect(dev.sessionOpen).toBe(true); // the ABORT was lost too
    dev.mute.clear();
    await s.put(1, 'b', bytes(8192));
    expect(dev.files.get('b')).toEqual(bytes(8192));
    expect(lines.some((l) => /ABORT, then BEGIN/.test(l))).toBe(true);
  });

  test('a wrong expected hash is refused by the device (E_HASH, no file); a wrong END hash throws', async () => {
    const dev = new FakeBulkDevice();
    const data = bytes(5000);
    await expect(session(dev).put(1, 'f', data, { expected: hash(data) ^ 2 })).rejects.toMatchObject({
      status: 'E_HASH',
    });
    expect(dev.files.size).toBe(0);
    expect(dev.sessionOpen).toBe(false);
    dev.corruptEndHash = true;
    await expect(session(dev).put(1, 'f', data, { expected: hash(data) })).rejects.toThrow(/not the expected/);
  });

  test('without REPLACE an existing file is refused; with it a failed upload keeps the old file and a good one swaps it at END', async () => {
    const dev = new FakeBulkDevice();
    dev.files.set('f', Uint8Array.of(1));
    const e = await session(dev)
      .put(1, 'f', bytes(512))
      .catch((x) => x);
    assert(e instanceof BulkDeviceError);
    expect(e.status).toBe('L_E_EXISTS');
    expect(dev.files.get('f')).toEqual(Uint8Array.of(1));
    dev.mute.add(BULK_CMD.DATA);
    await expect(session(dev).put(1, 'f', bytes(8192), { replace: true })).rejects.toMatchObject({ kind: 'timeout' });
    expect(dev.files.get('f')).toEqual(Uint8Array.of(1));
    dev.mute.clear();
    const s = session(dev);
    await expect(s.put(1, 'f', bytes(600), { replace: true, expected: hash(bytes(600)) ^ 2 })).rejects.toMatchObject({
      status: 'E_HASH',
    });
    expect(dev.files.get('f')).toEqual(Uint8Array.of(1));
    await s.put(1, 'f', bytes(600), { replace: true });
    expect(dev.files.get('f')).toEqual(bytes(600));
    expect(dev.log.some(([c, fl]) => c === BULK_CMD.BEGIN && fl === BULK_FLAG.REPLACE)).toBe(true);
  });

  test('a late reply after a timeout is dropped and the next request is answered', async () => {
    const dev = new FakeBulkDevice();
    const lines: string[] = [];
    const s = session(dev, { log: (l: string) => lines.push(l) });
    dev.hold = true;
    expect((await timeoutOf(s.info())).kind).toBe('timeout');
    dev.release(); // the reply to the timed-out INFO arrives late
    expect((await s.info()).align).toBe(512);
    expect(lines.some((l) => /late reply/.test(l))).toBe(true);
    expect(s.outstanding).toBe(0);
  });

  test('a cancel during recv is `cancelled` (not a timeout), the outstanding replies are read, and the link stays in step', async () => {
    const dev = new FakeBulkDevice();
    dev.mute.add(BULK_CMD.DATA); // never answered: put waits in recv
    const ctl = new AbortController();
    const s = session(dev, { timeoutMs: 30_000 });
    const t0 = Date.now();
    const p = s.put(1, 'f', bytes(20 * 4096), { signal: ctl.signal });
    setTimeout(() => ctl.abort(), 40);
    expect((await timeoutOf(p)).kind).toBe('cancelled');
    expect(Date.now() - t0).toBeLessThan(5000);
    expect(dev.sessionOpen).toBe(false);
    dev.mute.clear();
    expect((await s.info()).align).toBe(512);
  });

  test('cancel mid-file: every outstanding reply (the head too) is read, ABORT is confirmed', async () => {
    const dev = new FakeBulkDevice();
    const ctl = new AbortController();
    const s = session(dev);
    const e = await s
      .put(1, 'f', bytes(20 * 4096), { signal: ctl.signal, onProgress: (a) => a >= 5 * 4096 && ctl.abort() })
      .catch((x) => x);
    expect(e.kind).toBe('cancelled');
    expect(dev.sessionOpen).toBe(false);
    expect(dev.files.size).toBe(0);
    expect(s.outstanding).toBe(0);
    await s.put(1, 'g', bytes(512));
  });

  test('a failed ABORT then a new put', async () => {
    const dev = new FakeBulkDevice();
    dev.mute.add(BULK_CMD.DATA);
    dev.mute.add(BULK_CMD.ABORT);
    const s = session(dev, { busyRetries: 1 });
    expect((await timeoutOf(s.put(1, 'f', bytes(8192)))).kind).toBe('timeout');
    expect(dev.sessionOpen).toBe(true);
    dev.mute.clear();
    await s.put(1, 'g', bytes(8192));
    expect(dev.sessionOpen).toBe(false);
  });

  test('a silent device times out and the session is aborted', async () => {
    const dev = new FakeBulkDevice();
    dev.mute.add(BULK_CMD.DATA);
    const e = await session(dev)
      .put(1, 'f', bytes(8192))
      .catch((x) => x);
    expect(e.kind).toBe('timeout');
    expect(dev.sessionOpen).toBe(false);
  });

  test('empty and short files, and VERIFY of a range', async () => {
    const dev = new FakeBulkDevice();
    const s = session(dev);
    await s.put(1, 'empty', new Uint8Array(), { expected: hash(new Uint8Array()) });
    const data = bytes(3 * 4096);
    await s.put(1, 'f', data);
    expect(await s.verify(4096, 4096)).toBe(hash(data.subarray(4096, 8192)));
    await expect(s.verify(0, 3 * 4096 + 1)).rejects.toMatchObject({ status: 'E_READ' });
  });

  test('a corrupt reply stream is fatal: the link is closed and nothing more is sent', async () => {
    let closed = 0;
    let sent = 0;
    const link: BulkLink = {
      send: async () => void sent++,
      recv: async () => new Uint8Array(12), // not a frame
      close() {
        closed++;
      },
    };
    const s = new BulkSession(link);
    await expect(s.info()).rejects.toThrow(/magic/);
    expect(closed).toBe(1);
    await expect(s.info()).rejects.toThrow(/magic/);
    expect(sent).toBe(1);
  });

  test('a frame that is a multiple of 512 bytes needs the zero-length packet', async () => {
    // BEGIN of exactly 512 bytes: 12 header + 4 root + path + NUL + 4 size + 4 hash
    const path = 'p'.repeat(512 - 12 - 4 - 1 - 8);
    const frame = bulkRequest(
      BULK_CMD.BEGIN,
      0,
      Uint8Array.from([0, 0, 0, 1, ...new TextEncoder().encode(path), 0, 0, 0, 0, 0, 0, 0, 0, 0]),
    );
    expect(frame.length).toBe(512);
    const dev = new FakeBulkDevice();
    dev.out(frame);
    expect(dev.log.length).toBe(0); // not delivered without the ZLP
    expect(dev.pendingOut).toBe(512);
    dev.out(new Uint8Array());
    expect(dev.log.length).toBe(1);
    const dev2 = new FakeBulkDevice();
    await session(dev2).put(1, path, new Uint8Array());
    expect(dev2.files.has(path)).toBe(true);
    expect(dev2.badFrames).toBe(0);
  });

  test('a 512-byte reply is followed by a zero-length IN transfer, which is no bytes', () => {
    const dev = new FakeBulkDevice();
    const reply = bulkReply(1, 0, 0, new Uint8Array(500));
    expect(reply.length).toBe(512);
    const t = dev.inTransfers(reply);
    expect(t.map((x) => x.length)).toEqual([512, 0]);
    const frames = new BulkFrames();
    expect(t.flatMap((c) => frames.feed(c))).toEqual([reply]);
  });

  test('a reply to another request is dropped, and a reply that never matches times out', async () => {
    const link: BulkLink = {
      send: async () => {},
      recv: async () => bulkReply(BULK_CMD.INFO, 99, 0, new Uint8Array(20)),
      close() {},
    };
    expect((await timeoutOf(new BulkSession(link, { timeoutMs: 30 }).info())).kind).toBe('timeout');
  });
});

describe('the nonce handshake (chooseBulk)', () => {
  const sysexWith = (devices: FakeBulkDevice[], target: number) => ({
    usbFastNonce: async (n: Uint8Array) => devices[target]!.setNonce(n),
  });
  const openerOf = (devices: FakeBulkDevice[]) => ({
    candidates: async () => devices.map((_, i) => ({ id: `1:${i}`, name: `dev${i}` })),
    open: async (id: string) => devices[Number(id.split(':')[1])]!,
  });

  test('two candidates: the one that returns the nonce is chosen, the other is closed', async () => {
    const devs = [new FakeBulkDevice(), new FakeBulkDevice()];
    for (const target of [0, 1]) {
      for (const d of devs) d.closed = false;
      const notes: string[] = [];
      const c = await chooseBulk(sysexWith(devs, target), openerOf(devs), (n) => notes.push(n), { timeoutMs: 20 });
      expect(c?.id).toBe(`1:${target}`);
      // candidates are tried in order: one before the match was opened and closed, one after it is never opened
      expect(devs[1 - target]!.closed).toBe(target === 1);
      expect(devs[target]!.closed).toBe(false);
    }
  });

  test('no candidate returns the nonce: undefined, with the reason; all are closed', async () => {
    const devs = [new FakeBulkDevice(), new FakeBulkDevice()];
    const notes: string[] = [];
    const sysex = { usbFastNonce: async () => {} }; // the MIDI target is a third device
    expect(await chooseBulk(sysex, openerOf(devs), (n) => notes.push(n), { timeoutMs: 20 })).toBeUndefined();
    expect(notes.at(-1)).toMatch(/none of 2 candidate/);
    expect(devs.every((d) => d.closed)).toBe(true);
  });

  test('an OUT-only candidate that never answers INFO is skipped within the probe bound', async () => {
    const waits: number[] = [];
    const silent: BulkLink = {
      send: async () => {},
      recv: async (ms) => (waits.push(ms), null),
      close() {},
    };
    const dev = new FakeBulkDevice();
    const opener = {
      candidates: async () => [],
      open: async () => dev,
      outOnlyCandidates: async () => [{ id: 'a' }, { id: 'b' }],
      openOutOnly: async (id: string) => (id === 'a' ? silent : dev),
    };
    const sysex = {
      usbFastNonce: async (n: Uint8Array) => dev.setNonce(n),
      usbFastReplies: async () => new Uint8Array(),
    };
    const c = await chooseBulk(sysex, opener, () => {}, { timeoutMs: 60_000 });
    expect(c?.id).toBe('b');
    expect(waits.length).toBe(1);
    expect(waits[0]).toBeLessThanOrEqual(2000);
  });

  test('no candidates is quiet; a nonce that is refused is a logged reason', async () => {
    const notes: string[] = [];
    expect(await chooseBulk({ usbFastNonce: async () => {} }, openerOf([]), (n) => notes.push(n))).toBeUndefined();
    expect(notes).toEqual([]);
    const sysex = {
      usbFastNonce: async () => {
        throw new Error('E_PROTO');
      },
    };
    expect(await chooseBulk(sysex, openerOf([new FakeBulkDevice()]), (n) => notes.push(n))).toBeUndefined();
    expect(notes[0]).toMatch(/did not take the nonce/);
  });
});

// ---- the samples backend ---------------------------------------------------------------------------------------------------------

/** The base backend's session: records what stays on SysEx. */
function basePort() {
  const calls: string[] = [];
  const port = {
    calls,
    info: async () => ({ caps: 0 }) as never,
    list: async () => (calls.push('list'), [] as never[]),
    mkdir: async (p: string) => void calls.push(`mkdir ${p}`),
    put: async (p: string) => void calls.push(`sysex-put ${p}`),
    delete: async (p: string) => void calls.push(`delete ${p}`),
    verify: async () => '',
    capacity: async () => [],
    rpc: async (_t: number, _p?: Uint8Array) => Uint8Array.of(1),
  };
  return port;
}
const prepared = (): Prepared => ({
  name: 'kick',
  bytes: bytes(2000),
  stored: Uint8Array.from([...bytes(2000), ...TRAILER]),
  kind: 'unchanged',
});

describe('openSamples with a bulk link', () => {
  test('digikit: put goes over bulk with the file the store holds, with no listing when the caller says; delete stays on SysEx', async () => {
    const dev = new FakeBulkDevice();
    const port = basePort();
    const backend = openSamples('digikit', { session: port as never, bulk: session(dev) });
    const f = prepared();
    await backend.put('drums', f, undefined, { exists: false });
    expect(dev.files.get('drums/kick')).toEqual(f.stored);
    expect(port.calls).toEqual([]); // no listing: the hint was enough
    // the hint says it exists: REPLACE, atomic, no delete over SysEx
    dev.files.set('drums/kick', Uint8Array.of(9));
    await backend.put('drums', f, undefined, { exists: true });
    expect(dev.files.get('drums/kick')).toEqual(f.stored);
    expect(port.calls).toEqual([]);
    // a failed replace keeps the old file
    dev.files.set('drums/kick', Uint8Array.of(9));
    dev.mute.add(BULK_CMD.DATA);
    expect(backend.bulkState).toBe('bulk');
    await backend.put('drums', f, undefined, { exists: true }); // the link failed: SysEx took it
    expect(port.calls).toEqual(['sysex-put samples/drums/kick']);
    expect(dev.files.get('drums/kick')).toEqual(Uint8Array.of(9)); // the bulk upload did not touch it
    // a stale listing (hint: new, but the file is there): E_EXISTS is reported, not hidden by a fallback
    const dev2 = new FakeBulkDevice();
    dev2.files.set('drums/kick', Uint8Array.of(9));
    const b2 = openSamples('digikit', { session: basePort() as never, bulk: session(dev2) });
    await expect(b2.put('drums', f, undefined, { exists: false })).rejects.toMatchObject({ status: 'L_E_EXISTS' });
    expect(b2.bulkState).toBe('bulk');
    port.calls.length = 0;
    await backend.remove('drums', 'kick');
    expect(port.calls).toEqual(['delete samples/drums/kick']);
  });

  test('a bulk failure mid-run: the rest of the run goes over SysEx, the failure is logged, and a fresh backend tries bulk again', async () => {
    const dev = new FakeBulkDevice();
    const port = basePort();
    const notes: string[] = [];
    const backend = openSamples('digikit', { session: port as never, bulk: session(dev), note: (n) => notes.push(n) });
    await backend.put('', prepared(), undefined, { exists: false });
    expect(backend.bulkState).toBe('bulk');
    dev.mute.add(BULK_CMD.DATA);
    const f2 = { ...prepared(), name: 'snare' };
    await backend.put('', f2, undefined, { exists: false });
    expect(backend.bulkState).toBe('fallback');
    expect(notes[0]).toMatch(/USB bulk link failed.*SysEx/);
    dev.mute.clear();
    await backend.put('', { ...prepared(), name: 'hat' }, undefined, { exists: false }); // still the fallback: no more bulk this run
    expect(port.calls).toEqual(['sysex-put samples/snare', 'sysex-put samples/hat']);
    expect(dev.files.has('hat')).toBe(false);
    // the next run builds the backend again and bulk works
    const next = openSamples('digikit', { session: port as never, bulk: session(dev) });
    await next.put('', { ...prepared(), name: 'tom' }, undefined, { exists: false });
    expect(dev.files.has('tom')).toBe(true);
  });

  test('defer: put reports the pending files, commit names them; an old device and a failed link give no result', async () => {
    const dev = new FakeBulkDevice();
    dev.batch = true;
    const backend = openSamples('digikit', { session: basePort() as never, bulk: session(dev) });
    const f = prepared();
    expect(await backend.put('d', f, undefined, { exists: false, defer: true })).toEqual({ pending: 1 });
    expect(await backend.put('d', { ...f, name: 'snare' }, undefined, { exists: false, defer: true })).toEqual({
      pending: 2,
    });
    expect(dev.files.size).toBe(0);
    await backend.commit!();
    expect([...dev.files.keys()]).toEqual(['d/kick', 'd/snare']);
    // no defer asked: nothing reported
    expect(await backend.put('d', { ...f, name: 'hat' }, undefined, { exists: false })).toBeUndefined();
    // an old device ignores DEFER and sends 4 bytes: committed, no result
    const old = new FakeBulkDevice();
    const b2 = openSamples('digikit', { session: basePort() as never, bulk: session(old) });
    expect(await b2.put('d', f, undefined, { exists: false, defer: true })).toBeUndefined();
    expect(old.files.has('d/kick')).toBe(true);
    // the link fails: SysEx takes the file (not deferred) and commit does nothing
    const dead = new FakeBulkDevice();
    dead.batch = true;
    const port = basePort();
    const b3 = openSamples('digikit', { session: port as never, bulk: session(dead) });
    dead.mute.add(BULK_CMD.DATA);
    expect(await b3.put('d', f, undefined, { exists: false, defer: true })).toBeUndefined();
    expect(b3.bulkState).toBe('fallback');
    await b3.commit!();
    expect(dead.log.some(([c]) => c === BULK_CMD.COMMIT)).toBe(false);
  });

  test('a cancel reaches the bulk put and is not a fallback', async () => {
    const dev = new FakeBulkDevice();
    dev.mute.add(BULK_CMD.DATA);
    const port = basePort();
    const backend = openSamples('digikit', { session: port as never, bulk: session(dev, { timeoutMs: 30_000 }) });
    const ctl = new AbortController();
    setTimeout(() => ctl.abort(), 30);
    const e = await timeoutOf(backend.put('', prepared(), undefined, { exists: false, signal: ctl.signal }));
    expect(e.kind).toBe('cancelled');
    expect(backend.bulkState).toBe('bulk');
    expect(port.calls).toEqual([]);
  });

  test('stock-rpc: the folder is made over MidiRpc, the bytes go over bulk, the expected hash covers the trailer', async () => {
    const dev = new FakeBulkDevice();
    const port = basePort();
    const sent: Uint8Array[] = [];
    port.rpc = async (_t: number, p?: Uint8Array) => (sent.push(p ?? new Uint8Array()), Uint8Array.of(1));
    // the fake device hashes what it receives (no trailer), so give the backend a hashOf that does the same
    const backend = openSamples('stock-rpc', {
      session: port as never,
      bulk: session(dev),
      hashOf: (d) => hash(d.subarray(0, d.length - 16)),
    });
    const f = prepared();
    await backend.put('drums', f, undefined, { exists: false });
    expect(dev.files.get('drums/kick')).toEqual(f.bytes);
    expect(sent.length).toBe(1); // the mkdir
  });

  test('without the hint the backend lists once; without a bulk link the backend is the SysEx one', async () => {
    const dev = new FakeBulkDevice();
    const port = basePort();
    const backend = openSamples('digikit', { session: port as never, bulk: session(dev) });
    await backend.put('', prepared());
    expect(port.calls).toEqual(['list']);
    const plain = basePort();
    await openSamples('digikit', { session: plain as never }).put('', prepared());
    expect(plain.calls).toEqual(['sysex-put samples/kick']);
  });
});

describe('ProcessBulkLink', () => {
  test('a put over a byte stream that cuts frames anywhere (the dkbulk bridge)', async () => {
    const dev = new FakeBulkDevice();
    let emit: (c: Uint8Array) => void = () => {};
    const link = new ProcessBulkLink(
      (b) => {
        void dev.send(b).then(async () => {
          for (let r; (r = await dev.recv(0));) for (let i = 0; i < r.length; i += 7) emit(r.slice(i, i + 7));
        });
      },
      (cb) => (emit = cb),
    );
    const data = bytes(5 * 4096 + 3);
    const s = new BulkSession(link, { chunk: 4096, timeoutMs: 500 });
    expect(await s.put(1, 'f', data, { expected: hash(data) })).toBe(hash(data));
    expect(dev.files.get('f')).toEqual(data);
  });

  test('a corrupt stream fails the link: recv and send reject, the process end is closed', async () => {
    let emit: (c: Uint8Array) => void = () => {};
    let ended = 0;
    const link = new ProcessBulkLink(
      () => {},
      (cb) => (emit = cb),
      () => void ended++,
    );
    const waiting = link.recv(1000);
    emit(new Uint8Array(12));
    await expect(waiting).rejects.toThrow(/magic/);
    expect(ended).toBe(1);
    await expect(link.recv(10)).rejects.toThrow(/magic/);
    await expect(link.send(new Uint8Array(12))).rejects.toThrow(/magic/);
  });
});

test('the file hash vector the Rust side asserts (dksample file_hash of abc)', () => {
  expect([...fileHash(new TextEncoder().encode('abc')).subarray(0, 4)]).toEqual([0x49, 0xbe, 0xc8, 0x45]);
  expect(BULK_STATUS.E_HASH).toBe(-56);
});
