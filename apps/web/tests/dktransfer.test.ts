// The pure TypeScript protocol client (`DkSession`, dkupload-core.ts) and the Transfer logic (dktransfer.ts) against a fake device that
// speaks the SysEx protocol of docs/design/digikit-upload.md byte for byte (framing included), as the Rust mock does for the wasm client.
// The fit rule is a stand-in here; the merged list with the real rule is dktransfer.wasm.test.ts.
import assert from 'node:assert/strict';
import {
  DkError,
  DkSession,
  decode8in7,
  encode8in7,
  frame,
  parseReply,
  restarted,
  splitMessages,
  storeSpace,
  TABLE_VERSION,
  type DkTransport,
} from '@digi/dk-session';
import { CAP, CMD, LOG_KIND } from '@digi/dk-session/gen_proto';
import type { Prepared } from '@digi/dk-session/dksample';
import { openSamples } from '@digi/dk-session/samples';
import { test } from 'vitest';
import {
  PROGRESS_EVENTS,
  buildView,
  deleteFiles,
  groupOf,
  hasHeaderTable,
  install,
  libraryModule,
  refreshServices,
  restartDevice,
  uploadProgress,
  type InstallDeps,
} from '@digi/workbench-core/modules/dktransfer.ts';
import { setFitReader } from '@digi/workbench-core/modules/fit.ts';
import { nameHash, readProvides, serviceFileOf, type ServiceFile } from '@digi/workbench-core/modules/services.ts';
import { fileHashHex } from '@digi/dk-session';
import { parseSoftwareVersion } from '@digi/workbench-core/transfer/connection-core.ts';
import { dkm, everythingFits, needsOf } from './support/dkm.ts';
import { FakeDevice, concat, hex, sha } from './support/fakedevice.ts';

setFitReader(everythingFits);

const session = (dev: DkTransport, o: Partial<ConstructorParameters<typeof DkSession>[1]> = {}) =>
  new DkSession(dev, { timeoutMs: 20, retries: 1, ...o });
const sleep = async () => {};

test('framing: the vectors of the spec, round trips, partial groups, message splitting', () => {
  // `80 01 02 03 04 05 06` packs to `40 00 01 02 03 04 05 06`; `FF` packs to `40 7F` (tests/digikit/test_dkupload.py has the same)
  assert.equal(hex(frame(CMD.DATA, 5, Uint8Array.from([0x80, 1, 2, 3, 4, 5, 6]))), 'f07d646b0111054000010203040506f7');
  assert.equal(hex(frame(CMD.INFO, 1, Uint8Array.of(0xff))).slice(14, -2), '407f');
  for (const n of [0, 1, 6, 7, 8, 13, 14, 15, 100]) {
    const b = Uint8Array.from({ length: n }, (_, i) => (i * 97 + 13) & 255);
    assert.deepEqual(decode8in7(encode8in7(b)), b, `${n} bytes`);
  }
  const reply = concat(
    [0xf0, 0x7d, 0x64, 0x6b, 1, CMD.DELETE | 0x40, 9],
    encode8in7(Uint8Array.of(0xc5, 0x12, 0x80)),
    [0xf7],
  );
  const r = parseReply(reply)!;
  assert.deepEqual([r.cmd, r.seq, r.status, [...r.body]], [CMD.DELETE, 9, -59, [0x12, 0x80]]);
  assert.equal(parseReply(frame(CMD.INFO, 1)), null); // a request is not a reply
  assert.equal(parseReply(Uint8Array.from([0xf0, 0, 0x20, 0x3c, 0x10, 0, 1, 0, 0xf7])), null); // the stock MidiRpc
  const s = splitMessages(Uint8Array.from([0x90, 1, 2, 0xf0, 1, 0xf7, 0xf0, 2, 3]));
  assert.deepEqual(
    s.messages.map((m) => [...m]),
    [[0xf0, 1, 0xf7]],
  );
  assert.deepEqual([...s.rest], [0xf0, 2, 3]);
});

test('header table: versions 1 to TABLE_VERSION are read, a service is grouped as Services, other versions are an older runtime', () => {
  assert.equal(groupOf(dkm('x/m', { machine: 1, tableVersion: 2 })), 'Machines');
  assert.equal(hasHeaderTable(dkm('x/m', { machine: 1, tableVersion: 2 })), true);
  assert.equal(groupOf(dkm('x/s', { role: 1, tableVersion: 2 })), 'Services');
  assert.equal(groupOf(dkm('x/s', { machine: 1, role: 1 })), 'Services');
  assert.equal(groupOf(dkm('x/t')), 'Tools');
  for (const tableVersion of [0, TABLE_VERSION + 1]) {
    const f = dkm('x/o', { machine: 1, role: 1, tableVersion });
    assert.equal(hasHeaderTable(f), false, `version ${tableVersion}`);
    assert.equal(groupOf(f), 'Tools', `version ${tableVersion}`);
  }
});

test('session: a runtime without the file hash capability is refused at once, before any file command', async () => {
  const dev = new FakeDevice();
  dev.caps = Object.values(CAP).reduce((a: number, bit) => (bit === CAP.FILEHASH ? a : a | (1 << bit)), 0);
  const s = session(dev);
  const want = (e: unknown) =>
    e instanceof DkError && e.message === 'Update the firmware: this runtime uses the old store format';
  await assert.rejects(s.put('modules/a.dkm', dkm('x/0')), want);
  await assert.rejects(s.list(), want);
  await assert.rejects(s.verify('modules/a.dkm'), want);
  assert.equal(dev.files.size, 0);
});

test('session: info with caps, list in pages, modules, capacity, read, verify, select', async () => {
  const dev = new FakeDevice();
  for (const [i, n] of ['modules/a.dkm', 'modules/b.dkm', 'modules/c.dkm'].entries())
    dev.files.set(n, dkm(`x/${i}`, { salt: i }));
  dev.loaded = ['someone/machine'];
  const s = session(dev);
  const info = await s.info();
  assert.deepEqual(
    [info.port, info.runtime, info.max_chunk, info.safe_mode, info.card_read_only],
    ['dt2', 0x204, 32768, false, false],
  );
  assert.ok(info.caps !== null);
  assert.equal(info.image_id, null, 'a runtime before 0.5.2 sends no image id');
  dev.imageId = '281e6b9b1a0a9b83';
  assert.equal((await s.info()).image_id, '281e6b9b1a0a9b83');
  const ls = await s.list();
  assert.deepEqual(
    ls.map((e) => e.path),
    ['modules/a.dkm', 'modules/b.dkm', 'modules/c.dkm'],
  ); // three entries, two per page
  assert.equal(ls[0].hash, hex(sha(dev.files.get('modules/a.dkm')!)));
  assert.deepEqual(
    (await s.modules()).map((m) => [m.name, m.types, m.needs]),
    [['someone/machine', [17], 0x204]],
  );
  assert.deepEqual(await s.capacity(), {
    MODULES: { limit: 8, used: 1, largest: 0 },
    ARENA: { limit: 1000, used: 0, largest: 300 },
    DSP_AREA: { limit: 1000, used: 0, largest: 50 },
  });
  const h = await s.header('modules/b.dkm');
  assert.equal(h.length, 0xa0);
  assert.deepEqual(h, dev.files.get('modules/b.dkm')!.subarray(0, 0xa0));
  assert.equal(await s.verify('modules/b.dkm'), hex(sha(dev.files.get('modules/b.dkm')!)));
  await s.select(['modules/a.dkm', 'modules/c.dkm']);
  assert.deepEqual(dev.selection, ['modules/a.dkm', 'modules/c.dkm']);
  await assert.rejects(s.verify('modules/none.dkm'), (e) => e instanceof DkError && e.status === 'E_NOFILE');
  await assert.rejects(s.select(Array(9).fill('modules/x.dkm')), /at most 8/);
});

test('session: a timeout sends the request again; upload in chunks with progress, a repeated DATA, E_HASH', async () => {
  const dev = new FakeDevice();
  // the first request gets no answer: sent again with a new seq
  dev.silent = 1;
  const s = session(dev, { chunk: 512 });
  assert.equal((await s.info()).port, 'dt2');
  await assert.rejects(
    session(Object.assign(new FakeDevice(), { silent: 9 })).info(),
    (e) => e instanceof DkError && e.kind === 'timeout',
  );
  const data = dkm('x/big', { size: 2000 });
  const seen: number[][] = [];
  await s.put('modules/big.dkm', data, (sent, total) => seen.push([sent, total]));
  assert.deepEqual(seen, [
    [512, 2000],
    [1024, 2000],
    [1536, 2000],
    [2000, 2000],
  ]);
  assert.deepEqual(dev.files.get('modules/big.dkm'), data);
  // a file that is not what BEGIN promised: the device says E_HASH (a repeated chunk would be answered ok)
  const bad = new Uint8Array(1000).fill(7);
  const dev2 = new FakeDevice();
  const orig = dev2.handle.bind(dev2);
  dev2.handle = (cmd, seq, body) => {
    if (cmd === CMD.DATA && new DataView(body.buffer, body.byteOffset).getUint32(0) === 0)
      body = concat(body.subarray(0, 4), new Uint8Array(512).fill(9));
    return orig(cmd, seq, body);
  };
  await assert.rejects(
    session(dev2, { chunk: 512 }).put('modules/bad.dkm', bad),
    (e) => e instanceof DkError && e.status === 'E_HASH',
  );
  assert.equal(dev2.files.size, 0);
  assert.ok(dev2.seen.includes('ABORT')); // after an error the upload is dropped
});

test('restarted(): the log of the old boot holds the RESTART request, the new boot starts with BOOT_START', () => {
  const boot = { time: 0, seq: 1, kind: LOG_KIND.BOOT_START, flags: 0, args: [4, 0, 0, 0, 0] };
  const asked = { time: 0, seq: 2, kind: LOG_KIND.UPLOAD, flags: 0, args: [CMD.RESTART, 0, 0, 0, 0] };
  assert.equal(restarted([boot, { ...asked, args: [CMD.SELECT, 0, 0, 0, 0] }]), true);
  assert.equal(restarted([boot, asked]), false);
  assert.equal(restarted([]), false);
  assert.throws(
    () => restarted([boot, asked, { ...asked, flags: 1, args: [CMD.RESTART, -43 >>> 0, 0, 0, 0] }]),
    /refused/,
  );
});

test('buildView reads the device: INFO, LIST, MODULES, CAPACITY and the headers of the card-only files by READ', async () => {
  const dev = new FakeDevice();
  const mach = dkm('someone/machine', { machine: 1, dsp: 64, size: 900 });
  dev.files.set('modules/machine.dkm', mach);
  dev.files.set('modules/other.dkm', dkm('someone/effect', { fx: 1, size: 700 }));
  dev.loaded = ['someone/machine'];
  const view = await buildView(session(dev), [(await libraryModule(mach, needsOf))!]);
  assert.deepEqual(
    [view.device, view.runtime, view.free_bytes, view.loaded],
    ['dt2', '0.2.4', 1 << 20, ['someone/machine']],
  );
  assert.equal(view.modules[0].on_card, 'yes');
  assert.deepEqual([view.card_only[0].group, view.card_only[0].version], ['Effects', '0.1.2']);
  assert.ok(dev.seen.includes('READ') && dev.seen.includes('MODULES') && dev.seen.includes('CAPACITY'));
});

test('a card-only module without the header table of this runtime says it was built for an older runtime', async () => {
  const dev = new FakeDevice();
  const old = dkm('someone/old', { machine: 1, size: 900 });
  old.fill(0, 0xa0, 0xa8); // no magic: the table of runtime 0.7
  dev.files.set('modules/old.dkm', old);
  dev.files.set('modules/new.dkm', dkm('someone/new', { machine: 1, size: 900 }));
  const view = await buildView(session(dev), []);
  const by = Object.fromEntries(view.card_only.map((r) => [r.path, r]));
  assert.deepEqual(by['modules/old.dkm'].warnings, ['module built for an older runtime: rebuild it']);
  assert.equal(by['modules/new.dkm'].warnings, undefined);
});

test('Modules mode lists the modules/ folder only: the samples/ of a version 3 store stay out (with and without FOLDERS)', async () => {
  const mach = dkm('someone/machine', { machine: 1, dsp: 64, size: 900 });
  const withSamples = () => {
    const dev = new FakeDevice();
    dev.files.set('modules/machine.dkm', mach);
    dev.files.set('modules/other.dkm', dkm('someone/effect', { fx: 1, size: 700 }));
    dev.files.set('samples/01', new Uint8Array(3000).fill(7));
    dev.files.set('samples/ff_ht_bass_drop_A#', new Uint8Array(500).fill(9));
    dev.files.set('samples/Drums/kick.wav', new Uint8Array(400).fill(3));
    dev.dirs.add('samples/empty');
    return dev;
  };
  const lib = [(await libraryModule(mach, needsOf))!];
  const rows = (v: { modules: object[]; card_only: object[] }) =>
    [...v.modules, ...v.card_only]
      .map((r) => (r as { path?: string; id: string }).path ?? (r as { id: string }).id)
      .sort();
  // FOLDERS: the device filters by the LIST prefix, and the host does not read the other files' headers
  const dev = withSamples();
  const view = await buildView(session(dev), lib);
  assert.deepEqual(rows(view), ['modules/other.dkm', 'modules/machine.dkm'].sort());
  assert.deepEqual(
    view.card_only.map((r) => r.name),
    ['someone/effect'],
  );
  // a runtime without FOLDERS answers every entry to a LIST: the host filters
  const old = withSamples();
  old.caps = Object.values(CAP).reduce((a: number, bit) => a | (1 << bit), 0) & ~(1 << CAP.FOLDERS);
  assert.deepEqual(rows(await buildView(session(old), lib)), ['modules/other.dkm', 'modules/machine.dkm'].sort());
  // install compares with the modules/ entries too, and leaves the samples alone
  const events: any[] = [];
  await install(session(dev), ['someone/machine'], lib, (e) => events.push(e));
  assert.ok(events.some((e) => e.event === 'skip'));
  assert.equal(dev.files.size, 5);
});

test('free space above 4 GiB: INFO saturates at 2^32 - 1, CAPACITY STORE (KiB) does not, and the view uses it', async () => {
  const dev = new FakeDevice();
  dev.free = 0xffffffff; // what the runtime sends for a 19 GiB store
  dev.storeKib = 20_000_000; // 19 GiB: more than a u32 holds in bytes
  const s = session(dev);
  assert.equal((await s.info()).free_bytes, 4294967295);
  assert.deepEqual((await s.capacity()).STORE, { limit: 20_000_000, used: 1024, largest: 0 });
  assert.deepEqual(await storeSpace(s), { free: (20_000_000 - 1024) * 1024, total: 20_000_000 * 1024 });
  const view = await buildView(s, []);
  assert.equal(view.free_bytes, (20_000_000 - 1024) * 1024);
  assert.ok(view.free_bytes > 2 ** 32);
  // a runtime without the STORE entry (or without CAPACITY): INFO's bytes
  dev.storeKib = null;
  assert.deepEqual(await storeSpace(s), { free: 4294967295, total: null });
  assert.equal((await buildView(s, [])).free_bytes, 4294967295);
});

test('LOADS version 2: a module the boot plan refused says what is full and is tagged refused', async () => {
  const dev = new FakeDevice();
  dev.files.set('modules/pluck.dkm', dkm('someone/pluck', { size: 900 }));
  // state REFUSED (2), result E_PLAN_FIT (-78), detail: capacity kind 6 (machine rows)
  dev.loadFailures = [{ name: 'pluck.dkm', result: -78, state: 2, detail: [6, 0] }];
  const s = session(dev);
  assert.deepEqual(await s.loadStates(), [
    { index: 0, name: 'pluck.dkm', state: 'REFUSED', result: -78, detail: [6, 0] },
  ]);
  assert.deepEqual(await s.loadFailures(), [{ name: 'pluck.dkm', result: -78 }]);
  const row = (await buildView(s, [])).card_only.find((r) => r.path === 'modules/pluck.dkm')!;
  assert.equal(
    row.not_loaded,
    'pluck refused: no room: the machine rows that the earlier modules of the selection left is not enough',
  );
  assert.equal(row.load_state, 'REFUSED');
});

test('LOADS: the view says why a selected module is not loaded (the file name the runtime kept, the module it needs)', async () => {
  const dev = new FakeDevice();
  dev.files.set('modules/usb-fast.dkm', dkm('digikit/usb-fast', { size: 900 }));
  dev.loadFailures = [{ name: 'usb-fast.dkm', result: -7, needs: 'usb-bulk' }];
  const s = session(dev);
  assert.deepEqual(await s.loadFailures(), [{ name: 'usb-fast.dkm', result: -7, needs: 'usb-bulk' }]);
  const row = (await buildView(s, [])).card_only.find((r) => r.path === 'modules/usb-fast.dkm')!;
  assert.equal(
    row.not_loaded,
    'usb-fast not loaded: this firmware has no core/usb-bulk. Flash firmware from the Firmware page.',
  );
});

test('install: skip the same file hash, upload the rest with capped progress, VERIFY each, no SELECT', async () => {
  const dev = new FakeDevice();
  const mach = dkm('someone/machine', { machine: 1, size: 900 });
  const big = dkm('test/big', { size: 20000 });
  dev.files.set('modules/machine.dkm', mach); // already on the card
  const events: any[] = [];
  const out = await install(
    session(dev, { chunk: 512 }),
    ['someone/machine', 'test/big'],
    [(await libraryModule(mach, needsOf))!, (await libraryModule(big, needsOf))!],
    (e) => events.push(e),
  );
  assert.deepEqual(out, { uploaded: ['test/big'], selected: [], restarted: false });
  assert.deepEqual(dev.files.get('modules/big.dkm') ?? dev.files.get('modules/big.dkm'), big);
  assert.ok(!dev.seen.includes('SELECT') && !dev.seen.includes('RESTART'));
  assert.equal(dev.seen.filter((c) => c === 'VERIFY').length, 2);
  const kinds = events.map((e) => e.event);
  assert.deepEqual(kinds.slice(0, 4), ['start', 'upload', 'skip', 'verify']); // the skipped file jumps straight to its verify
  assert.equal(kinds.at(-1), 'done');
  assert.deepEqual(events.at(-1), { event: 'done', restarted: false, selected: [] });
  const prog = events.filter((e) => e.event === 'progress');
  assert.ok(prog.length > 1 && prog.length <= PROGRESS_EVENTS + 1, String(prog.length)); // 40 chunks, a few events
  assert.deepEqual([prog.at(-1).sent, prog.at(-1).bytes], [20000, 20000]);
  assert.ok(prog.every((e) => e.id === 'test/big'));
  assert.deepEqual(
    events.filter((e) => e.event === 'verify').map((e) => e.ok),
    [true, true],
  );
  await assert.rejects(
    install(session(dev), ['someone/none'], [(await libraryModule(mach, needsOf))!]),
    /not a run-time module/,
  );
});

test('install: a VERIFY that does not match is an error', async () => {
  const dev = new FakeDevice();
  dev.corrupt = true;
  const m = dkm('someone/machine', { machine: 1 });
  const events: any[] = [];
  await assert.rejects(
    install(session(dev), ['someone/machine'], [(await libraryModule(m, needsOf))!], (e) => events.push(e)),
    /did not keep the file/,
  );
  assert.deepEqual(events.at(-1), { event: 'verify', id: 'someone/machine', path: 'modules/machine.dkm', ok: false });
});

test('the upload progress callback: at most a tenth apart, always the last', () => {
  const ev: any[] = [];
  const cb = uploadProgress((e) => ev.push(e), 'a/b', 700);
  cb(700, 700);
  assert.deepEqual(ev, [{ event: 'progress', id: 'a/b', sent: 700, bytes: 700 }]);
  const many: any[] = [];
  const cb2 = uploadProgress((e) => many.push(e), 'a/c', 5);
  for (let n = 1; n <= 5; n++) cb2(n, 5);
  assert.equal(many.at(-1).sent, 5);
  assert.ok(many.length <= PROGRESS_EVENTS + 1);
});

test('delete: one DELETE per path, confirmed by LIST; a path that is not on the card sends nothing', async () => {
  const dev = new FakeDevice();
  for (const n of ['a', 'b', 'c']) dev.files.set(`modules/${n}.dkm`, dkm(`x/${n}`));
  const events: any[] = [];
  const out = await deleteFiles(session(dev), ['modules/a.dkm', 'modules/c.dkm', 'modules/a.dkm'], (e) =>
    events.push(e),
  );
  assert.deepEqual(out, { deleted: ['modules/a.dkm', 'modules/c.dkm'] }); // a repeated path is one DELETE
  assert.deepEqual([...dev.files.keys()], ['modules/b.dkm']);
  assert.equal(dev.seen.filter((c) => c === 'DELETE').length, 2);
  assert.ok(!dev.seen.includes('SELECT')); // the selection is the device's
  assert.deepEqual(
    events.map((e) => e.event),
    ['start', 'delete', 'delete', 'done'],
  );
  assert.deepEqual(events.at(-1), { event: 'done', deleted: ['modules/a.dkm', 'modules/c.dkm'] });
  const before = dev.seen.length;
  const dev2 = new FakeDevice();
  dev2.files.set('modules/a.dkm', dkm('x/a'));
  await assert.rejects(
    deleteFiles(session(dev2), ['modules/a.dkm', 'modules/none.dkm']),
    /not on the card: modules\/none\.dkm/,
  );
  assert.ok(!dev2.seen.includes('DELETE'));
  assert.ok(before > 0);
  // the card keeps the file after DELETE: error
  const dev3 = new FakeDevice();
  dev3.files.set('modules/a.dkm', dkm('x/a'));
  const orig = dev3.handle.bind(dev3);
  dev3.handle = (cmd, seq, body) => (cmd === CMD.DELETE ? dev3.reply(cmd, seq, 0) : orig(cmd, seq, body));
  await assert.rejects(deleteFiles(session(dev3), ['modules/a.dkm']), /still on the card after DELETE/);
});

// --- services: a .dks provides one service; a module or a service lists the services it needs in the NEEDS entry of its header table ---
const V1_0 = 0x010000;
const V1_2 = 0x010200;

/** A file of the module format with a header table (version 2): `role` in header word 0x38, NEEDS (hash, min version, hard) and PROVIDES (hash, version). */
function withTable(
  base: Uint8Array,
  role: number,
  needs: [number, number, boolean][],
  provides?: [number, number],
): Uint8Array {
  const b = base.slice();
  const v = new DataView(b.buffer);
  const entries: Uint8Array[] = [];
  const entry = (tag: number, count: number, payload: number[]): Uint8Array => {
    const e = new Uint8Array(8 + payload.length * 4);
    const ev = new DataView(e.buffer);
    ev.setUint16(0, tag);
    ev.setUint16(2, count);
    ev.setUint32(4, payload.length * 4);
    payload.forEach((w, i) => ev.setUint32(8 + i * 4, w));
    return e;
  };
  if (needs.length)
    entries.push(
      entry(
        1,
        needs.length,
        needs.flatMap(([h, min, hard]) => [h, min, hard ? 1 : 0]),
      ),
    );
  if (provides) entries.push(entry(9, 1, provides));
  let at = 0xa0;
  v.setUint32(at, 0x444b5442);
  v.setUint16(at + 4, 2);
  v.setUint16(at + 6, entries.length);
  at += 8;
  for (const e of entries) {
    b.set(e, at);
    at += e.length;
  }
  v.setUint32(0x38, role);
  v.setUint32(0x90, 0xa0);
  v.setUint32(0x94, at - 0xa0);
  return b;
}

/** An unbound service file of the workbench: `stem` provides the service of that name. */
function service(stem: string, needs: [number, number, boolean][] = []): ServiceFile {
  const bytes = withTable(dkm(`svc/${stem}`, { size: 800 }), 1, needs, [nameHash(stem), V1_2]);
  return serviceFileOf(stem, bytes, fileHashHex(bytes))!;
}

/** A module that hard-needs the services (by stem). */
const needing = (name: string, stems: string[]): Uint8Array =>
  withTable(
    dkm(name, { machine: 1, size: 900 }),
    0,
    stems.map((s) => [nameHash(s), V1_0, true]),
  );

/** The fake binder: the user's firmware hash is appended, so the bound file differs from the unbound one. */
const binder = {
  bind: async (b: Uint8Array, fw: string) => {
    // like patchkit bind: lfo hooks no firmware site and comes back as built; the others need the stock image (an empty hash: none is kept)
    if (readProvides(b)?.hash32 === nameHash('lfo')) return b;
    if (!fw) throw new Error('no stock firmware');
    return concat(b, Buffer.from(fw));
  },
};
const bound = (f: ServiceFile): Uint8Array => concat(f.bytes, Buffer.from('fw1'));

const depsOf = (services: ServiceFile[], over: Partial<InstallDeps> = {}): InstallDeps => ({
  services,
  binder,
  firmwareHash: 'fw1',
  runtimeIdentity: (info) => ({ version: info.runtime, fingerprint: null }),
  runtimeServices: new Set(),
  ...over,
});

test('install with services: a needed service is bound, uploaded and verified before its module', async () => {
  const fx = service('fx');
  const dev = new FakeDevice();
  const pluck = (await libraryModule(needing('someone/pluck', ['fx']), needsOf))!;
  const events: any[] = [];
  const out = await install(session(dev), ['someone/pluck'], [pluck], (e) => events.push(e), depsOf([fx]));
  assert.deepEqual(out.uploaded, ['someone/pluck']);
  assert.deepEqual(dev.files.get('services/fx.dks'), bound(fx));
  assert.deepEqual(dev.files.get('modules/pluck.dkm'), pluck.bytes);
  const uploads = events.filter((e) => e.event === 'upload').map((e) => [e.kind ?? 'module', e.path]);
  assert.deepEqual(uploads, [
    ['service', 'services/fx.dks'],
    ['module', 'modules/pluck.dkm'],
  ]);
  assert.ok(events.filter((e) => e.kind === 'service').every((e) => e.id === 'fx'));
  assert.deepEqual(
    events.filter((e) => e.event === 'verify').map((e) => [e.kind ?? 'module', e.ok]),
    [
      ['service', true],
      ['module', true],
    ],
  );
});

test('install with services: the same bound bytes on the card are not sent again', async () => {
  const fx = service('fx');
  const dev = new FakeDevice();
  dev.files.set('services/fx.dks', bound(fx));
  const pluck = (await libraryModule(needing('someone/pluck', ['fx']), needsOf))!;
  const events: any[] = [];
  await install(session(dev), ['someone/pluck'], [pluck], (e) => events.push(e), depsOf([fx]));
  assert.ok(events.some((e) => e.event === 'skip' && e.kind === 'service'));
  assert.ok(!events.some((e) => e.event === 'progress' && e.kind === 'service'));
  assert.ok(events.some((e) => e.event === 'verify' && e.kind === 'service' && e.ok));
});

test('install with services: a module the services cannot serve is named and skipped, the other ticked module installs', async () => {
  const fx = service('fx');
  const dev = new FakeDevice();
  const pluck = (await libraryModule(needing('someone/pluck', ['ghost']), needsOf))!;
  const shot = (await libraryModule(needing('someone/shot', ['fx']), needsOf))!;
  const events: any[] = [];
  const out = await install(
    session(dev),
    ['someone/pluck', 'someone/shot'],
    [pluck, shot],
    (e) => events.push(e),
    depsOf([fx]),
  );
  const refusals = events.filter((e) => e.event === 'refuse').map((e) => e.refusal);
  assert.deepEqual(
    refusals.map((r) => r.module),
    ['someone/pluck'],
  );
  assert.deepEqual(out.uploaded, ['someone/shot']);
  assert.deepEqual([...dev.files.keys()].sort(), ['modules/shot.dkm', 'services/fx.dks']);
  // every ticked module refused: only the refusals are emitted
  const only: any[] = [];
  const none = await install(session(new FakeDevice()), ['someone/pluck'], [pluck], (e) => only.push(e), depsOf([fx]));
  assert.deepEqual(
    only.map((e) => e.event),
    ['refuse'],
  );
  assert.deepEqual(none.uploaded, []);
});

test('install with services: a service that fails VERIFY stops the install before any module is uploaded', async () => {
  const fx = service('fx');
  const dev = new FakeDevice();
  dev.corrupt = true;
  const pluck = (await libraryModule(needing('someone/pluck', ['fx']), needsOf))!;
  await assert.rejects(
    install(session(dev), ['someone/pluck'], [pluck], () => {}, depsOf([fx])),
    /services\/fx\.dks: .*did not keep the file/,
  );
  assert.ok(!dev.files.has('modules/pluck.dkm'));
});

test('install with services: without a kept stock image a module that needs a service is refused, the others install', async () => {
  const fx = service('fx');
  const dev = new FakeDevice();
  const pluck = (await libraryModule(needing('someone/pluck', ['fx']), needsOf))!;
  const plain = (await libraryModule(needing('someone/plain', []), needsOf))!;
  const events: any[] = [];
  const out = await install(
    session(dev),
    ['someone/pluck', 'someone/plain'],
    [pluck, plain],
    (e) => events.push(e),
    depsOf([fx], { firmwareHash: null }),
  );
  assert.deepEqual(
    events.filter((e) => e.event === 'refuse').map((e) => [e.refusal.module, e.refusal.kind]),
    [['someone/pluck', 'no-stock']],
  );
  assert.deepEqual(out.uploaded, ['someone/plain']);
  assert.deepEqual([...dev.files.keys()], ['modules/plain.dkm']);
});

test('install with services: a service with no hook (lfo) comes back from the binder as built and needs no stock image', async () => {
  const lfo = service('lfo');
  const dev = new FakeDevice();
  const user = (await libraryModule(needing('someone/mod', ['lfo']), needsOf))!;
  const out = await install(session(dev), ['someone/mod'], [user], () => {}, depsOf([lfo], { firmwareHash: null }));
  assert.deepEqual(out.uploaded, ['someone/mod']);
  assert.deepEqual(dev.files.get('services/lfo.dks'), lfo.bytes);
});

test('buildView with services: a binder that fails shows the unbound rows and refuses the modules that need them', async () => {
  const fx = service('fx');
  const dev = new FakeDevice();
  const pluck = (await libraryModule(needing('someone/pluck', ['fx']), needsOf))!;
  const failing = { bind: async () => Promise.reject(new Error('no port for this firmware')) };
  const v = await buildView(session(dev), [pluck], depsOf([fx], { binder: failing }));
  assert.deepEqual(
    v.refusals?.map((r) => [r.module, r.kind]),
    [['someone/pluck', 'no-stock']],
  );
  assert.equal(v.modules[0]!.services?.[0]?.state, 'not on device');
});

test('install with services: the bound file is made once per firmware and unbound hash', async () => {
  const fx = service('fx');
  const cache = new Map<string, Uint8Array>();
  let calls = 0;
  const counting = {
    bind: async (b: Uint8Array, fw: string) => {
      calls++;
      return concat(b, Buffer.from(fw));
    },
  };
  const pluck = (await libraryModule(needing('someone/pluck', ['fx']), needsOf))!;
  const deps = depsOf([fx], { binder: counting, cache });
  await install(session(new FakeDevice()), ['someone/pluck'], [pluck], () => {}, deps);
  await install(session(new FakeDevice()), ['someone/pluck'], [pluck], () => {}, deps);
  assert.equal(calls, 1);
  // another firmware binds again
  await install(session(new FakeDevice()), ['someone/pluck'], [pluck], () => {}, { ...deps, firmwareHash: 'fw2' });
  assert.equal(calls, 2);
});

test('refreshServices (Flash): a .dks on the card with other bytes is replaced, an equal one and one the workbench lacks stay', async () => {
  const fx = service('fx');
  const lfo = service('lfo');
  const dev = new FakeDevice();
  dev.files.set('modules/m.dkm', needing('x/m', ['fx', 'lfo']));
  dev.files.set('services/fx.dks', Buffer.from('old build'));
  dev.files.set('services/lfo.dks', lfo.bytes);
  dev.files.set('services/other.dks', Buffer.from('not ours'));
  const events: any[] = [];
  const replaced = await refreshServices(session(dev), depsOf([fx, lfo]), [], (e) => events.push(e));
  assert.deepEqual(replaced, ['services/fx.dks']);
  assert.deepEqual(dev.files.get('services/fx.dks'), bound(fx));
  assert.deepEqual(dev.files.get('services/other.dks'), Buffer.from('not ours'));
  assert.ok(events.some((e) => e.event === 'verify' && e.kind === 'service' && e.ok));
  // no stock image: the binder cannot bind, nothing is touched
  assert.deepEqual(await refreshServices(session(dev), depsOf([fx], { firmwareHash: null }), []), []);
});

test('install with services: a .dks that no module on the card needs any more is deleted', async () => {
  const fx = service('fx');
  const old = service('old');
  const dev = new FakeDevice();
  dev.files.set('services/old.dks', old.bytes);
  const pluck = (await libraryModule(needing('someone/pluck', ['fx']), needsOf))!;
  const events: any[] = [];
  await install(session(dev), ['someone/pluck'], [pluck], (e) => events.push(e), depsOf([fx, old]));
  assert.deepEqual([...dev.files.keys()].sort(), ['modules/pluck.dkm', 'services/fx.dks']);
  assert.deepEqual(
    events.filter((e) => e.event === 'delete'),
    [{ event: 'delete', path: 'services/old.dks', why: 'unused-service' }],
  );
});

/** A card with pluck (needs fx and machines), shot (needs machines) and the two services. */
function cardWithServices(): FakeDevice {
  const dev = new FakeDevice();
  dev.files.set('modules/pluck.dkm', needing('someone/pluck', ['fx', 'machines']));
  dev.files.set('modules/shot.dkm', needing('someone/shot', ['machines']));
  dev.files.set('services/fx.dks', service('fx').bytes);
  dev.files.set('services/machines.dks', service('machines').bytes);
  return dev;
}

test('delete: a module takes its now-unused .dks with it and keeps the one another module needs', async () => {
  const dev = cardWithServices();
  const events: any[] = [];
  const out = await deleteFiles(session(dev), ['modules/pluck.dkm'], (e) => events.push(e));
  assert.deepEqual(out, { deleted: ['modules/pluck.dkm', 'services/fx.dks'] });
  assert.deepEqual([...dev.files.keys()].sort(), ['modules/shot.dkm', 'services/machines.dks']);
  assert.deepEqual(
    events.find((e) => e.event === 'keep'),
    { event: 'keep', path: 'services/machines.dks', by: ['modules/shot.dkm'] },
  );
  assert.deepEqual(
    events.find((e) => e.event === 'delete' && e.why),
    { event: 'delete', path: 'services/fx.dks', why: 'unused-service' },
  );
});

test('delete: a needed .dks named alone is refused before any DELETE', async () => {
  const dev = cardWithServices();
  await assert.rejects(
    deleteFiles(session(dev), ['services/machines.dks']),
    /machines service is needed by .*shot.*Delete that module first/,
  );
  assert.ok(!dev.seen.includes('DELETE'));
  assert.equal(dev.files.size, 4);
});

test('buildView with services: a library module lists its services with their state, a card .dks says what needs it', async () => {
  const fx = service('fx');
  const machines = service('machines');
  const dev = new FakeDevice();
  dev.files.set('services/fx.dks', bound(fx));
  dev.files.set('services/machines.dks', concat(machines.bytes, [9])); // another file than the bound one
  const pluck = (await libraryModule(needing('someone/pluck', ['fx', 'machines', 'ghost']), needsOf))!;
  const shot = (await libraryModule(needing('someone/shot', ['fx']), needsOf))!;
  dev.files.set('modules/shot.dkm', shot.bytes);
  const view = await buildView(session(dev), [pluck, shot], depsOf([fx, machines]));
  const rows = Object.fromEntries(view.modules.map((r): [string, typeof r] => [String(r.id), r]));
  assert.deepEqual(
    rows['someone/pluck'].services?.map((r) => [r.stem, r.state]),
    [
      ['fx', 'on device'],
      ['machines', 'differs'],
    ],
  );
  assert.equal(rows['someone/pluck'].service_refusal?.module, 'someone/pluck');
  assert.equal(rows['someone/shot'].service_refusal, undefined);
  assert.deepEqual(
    rows['someone/shot'].services?.map((r) => [r.stem, r.state]),
    [['fx', 'on device']],
  );
  assert.equal(view.refusals?.length, 1);
  const fxRow = view.card_only.find((r) => r.path === 'services/fx.dks')!;
  assert.deepEqual(fxRow.needed_by, ['modules/shot.dkm']);
});

test('restart: RESTART, wait for the new boot in the log, then report what MODULES lists', async () => {
  const dev = new FakeDevice();
  dev.selection = ['modules/machine.dkm'];
  const events: any[] = [];
  const out = await restartDevice(session(dev, { timeoutMs: 5, retries: 0 }), (e) => events.push(e), sleep);
  assert.deepEqual(out, { loaded: ['mod:modules/machine.dkm'], missing: [] });
  assert.deepEqual(
    events.map((e) => e.event),
    ['restart', 'wait', 'done'],
  );
  assert.deepEqual(events.at(-1), { event: 'done', restarted: true, loaded: ['mod:modules/machine.dkm'], missing: [] });
  // the old boot answered INFO first (the LOG capability: the log decides), then two requests got nothing, then the new boot's log
  assert.ok(dev.seen.filter((c) => c === 'INFO').length >= 2);
  assert.ok(dev.seen.includes('LOG'));
  assert.equal(dev.log.length, 1);
  assert.equal(dev.log[0].kind, LOG_KIND.BOOT_START);
});

test('LIST by prefix pages through the matches; MKDIR makes a folder entry that a file replaces', async () => {
  const dev = new FakeDevice();
  const s = session(dev);
  await s.put('modules/a.dkm', dkm('a'));
  await s.put('samples/one.wav', new Uint8Array(1500).fill(1));
  await s.put('samples/two.wav', new Uint8Array(700).fill(2));
  await s.put('samples/three.wav', new Uint8Array(900).fill(3));
  await s.mkdir('samples/empty');
  await s.mkdir('samples/empty'); // exists: ok
  const under = await s.list('samples/');
  assert.deepEqual(
    under.map((e) => [e.path, e.size]),
    [
      ['samples/empty', 0],
      ['samples/one.wav', 1500],
      ['samples/three.wav', 900],
      ['samples/two.wav', 700],
    ],
  );
  assert.equal(under[0].hash, '0'.repeat(64));
  assert.equal((await s.list()).length, 5);
  await s.put('samples/empty/x.wav', new Uint8Array(512));
  assert.deepEqual(
    (await s.list('samples/empty')).map((e) => e.path),
    ['samples/empty/x.wav'],
  );
  await assert.rejects(s.mkdir('modules/a.dkm'), (e) => e instanceof DkError && e.status === 'E_PATH');
  await s.delete('samples/one.wav');
  assert.equal((await s.list('samples/')).length, 3);
});

test('the samples listing of a folder with many files pages through and carries the hashes; a truncated reply gives a short listing, never a wrong match', async () => {
  const dev = new FakeDevice();
  const s = session(dev);
  const file = (i: number): Prepared => {
    const stored = new Uint8Array(40 + i).map((_, k) => (k * 3 + i) & 255);
    return { name: `s${i}`, bytes: stored.subarray(0, stored.length - 16), stored, kind: 'native' };
  };
  const samples = openSamples('digikit', { session: s });
  for (let i = 0; i < 25; i++) await samples.put('d', file(i));
  const all = await samples.list('d');
  assert.equal(all.length, 25, 'every page was read (two entries per reply)');
  for (let i = 0; i < 25; i++) assert.equal(samples.sameOnDevice(file(i), all), true, `s${i}`);
  // the same size, other bytes
  const other = file(3);
  other.stored = other.stored.map((b, k) => (k === 20 ? b ^ 1 : b));
  assert.equal(samples.sameOnDevice(other, all), false);
  // a reply that has no entries while the total says more: the listing is short, and the files it lacks are not "the same"
  dev.pageSize = 0;
  const cut = await samples.list('d');
  assert.equal(cut.length, 0);
  assert.equal(samples.sameOnDevice(file(1), cut), false);
});

test('VERIFY of a range, and verifyFile in pieces', async () => {
  const dev = new FakeDevice();
  const s = session(dev);
  const data = new Uint8Array(5000).map((_, i) => (i * 7) & 255);
  await s.put('samples/a.wav', data);
  assert.equal(await s.verify('samples/a.wav'), hex(sha(data)));
  assert.equal(await s.verify('samples/a.wav', 1024, 2000), hex(sha(data.subarray(1024, 3024))));
  assert.equal(await s.verifyFile('samples/a.wav', data, 1024), true);
  const other = data.slice();
  other[4999] ^= 1;
  assert.equal(await s.verifyFile('samples/a.wav', other, 1024), false);
  await assert.rejects(s.verify('samples/a.wav', 100, 10), (e) => e instanceof DkError && e.status === 'E_OFFSET');
  assert.equal(await s.verifyFile('samples/a.wav', new Uint8Array(0), 1024), false); // the file is not empty
});

test('session: the stock SoftwareVersion request is answered with the OS version string', async () => {
  const dev = new FakeDevice();
  dev.os = '1.17';
  const reply = await session(dev).rpc(2, new Uint8Array(), 100);
  assert.deepEqual(parseSoftwareVersion(reply), '1.17');
  await assert.rejects(session(new FakeDevice()).rpc(2, new Uint8Array(), 20), (e: DkError) => e.kind === 'timeout');
});
