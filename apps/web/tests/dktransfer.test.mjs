// The pure TypeScript protocol client (`DkSession`, dkupload-core.ts) and the Transfer logic (dktransfer.ts) against a fake device that
// speaks the SysEx protocol of docs/design/digikit-upload.md byte for byte (framing included), as the Rust mock does for the wasm client.
import assert from 'node:assert/strict';
import test from 'node:test';
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
} from '@digi/dk-session';
import { CAP, CMD, LOG_KIND } from '@digi/dk-session/gen_proto';
import {
  PROGRESS_EVENTS,
  buildView,
  deleteFiles,
  headerInfo,
  install,
  libraryModule,
  mergeView,
  restartDevice,
  uploadProgress,
} from '../src/modules/dktransfer.ts';
import { parseSoftwareVersion } from '../src/transfer/connection-core.ts';
import { FakeDevice, concat, hex, sha } from './fakedevice.mjs';
import { useFitWasm } from './fit-wasm.ts';

await useFitWasm();

/** A .dkm with a valid header (docs/design/digikit-dkm.md) of `size` bytes. */
function dkm(name, { machine = 0, fx = 0, service = 1, size = 700, dsp = 0, version = 0x000102, salt = 0 } = {}) {
  const b = new Uint8Array(size);
  b.set([0x44, 0x4b, 0x4d, 0x46], 0);
  new DataView(b.buffer).setUint32(8, size);
  new DataView(b.buffer).setUint32(0x34, version);
  b.set([1, 1, machine, 0, fx, 0, service, 0], 0x38);
  b.set(Buffer.from(name), 0x40);
  new DataView(b.buffer).setUint32(0x64, 100);
  new DataView(b.buffer).setUint32(0x68, 20);
  new DataView(b.buffer).setUint32(0x9c, dsp);
  for (let i = 0xa0; i < size; i++) b[i] = (i * 31 + salt) & 255;
  return b;
}

/**
 * The needs reader of these tests. The needs are inferred by Rust (packages/dkneeds, through patchkit.wasm: dkneeds.test.mjs runs the real one on
 * real files); a fake module here has only a header, so a stand-in gives the fit logic its input: the arena block of image 100 and bss 20, and the
 * DSP area the fake's DSP size word holds.
 */
const needsOf = (b) => {
  const dsp = new DataView(b.buffer, b.byteOffset, b.length).getUint32(0x9c);
  return { file_size: b.length, ARENA: 120, DSP_AREA: dsp, DSP_ALIGN: dsp ? 8 : 0 };
};

const session = (dev, o = {}) => new DkSession(dev, { timeoutMs: 20, retries: 1, ...o });
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
  const r = parseReply(reply);
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

test('session: a runtime without the file hash capability is refused at once, before any file command', async () => {
  const dev = new FakeDevice();
  dev.caps = Object.values(CAP).reduce((a, bit) => (bit === CAP.FILEHASH ? a : a | (1 << bit)), 0);
  const s = session(dev);
  const want = (e) =>
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
  assert.equal(ls[0].hash, hex(sha(dev.files.get('modules/a.dkm'))));
  assert.deepEqual(
    (await s.modules()).map((m) => [m.name, m.types, m.contracts[1]]),
    [['someone/machine', [17], [1, 0]]],
  );
  assert.deepEqual(await s.capacity(), {
    MODULES: { limit: 8, used: 1, largest: 0 },
    ARENA: { limit: 1000, used: 0, largest: 300 },
    DSP_AREA: { limit: 1000, used: 0, largest: 50 },
  });
  const h = await s.header('modules/b.dkm');
  assert.equal(h.length, 0xa0);
  assert.deepEqual(h, dev.files.get('modules/b.dkm').subarray(0, 0xa0));
  assert.equal(await s.verify('modules/b.dkm'), hex(sha(dev.files.get('modules/b.dkm'))));
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
  const seen = [];
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
  const boot = { seq: 1, kind: LOG_KIND.BOOT_START, flags: 0, args: [4, 0, 0, 0, 0] };
  const asked = { seq: 2, kind: LOG_KIND.UPLOAD, flags: 0, args: [CMD.RESTART, 0, 0, 0, 0] };
  assert.equal(restarted([boot, { ...asked, args: [CMD.SELECT, 0, 0, 0, 0] }]), true);
  assert.equal(restarted([boot, asked]), false);
  assert.equal(restarted([]), false);
  assert.throws(
    () => restarted([boot, asked, { ...asked, flags: 1, args: [CMD.RESTART, -43 >>> 0, 0, 0, 0] }]),
    /refused/,
  );
});

test('header, library module, the merged list', async () => {
  const mach = dkm('someone/machine', { machine: 1, dsp: 64, size: 900 });
  const sine = dkm('test/sine', { machine: 1, dsp: 400, size: 800 });
  const tool = dkm('someone/tool', { service: 1, size: 500 });
  assert.deepEqual([headerInfo(mach).group, headerInfo(mach).version], ['Machines', '0.1.2']);
  assert.equal(headerInfo(dkm('a', { fx: 1 })).group, 'Effects');
  assert.equal(headerInfo(tool).group, 'Tools');
  assert.equal(headerInfo(new Uint8Array(300)), null);
  assert.equal(await libraryModule(new Uint8Array(300)), null);
  const lm = await libraryModule(mach, needsOf);
  assert.deepEqual(
    [lm.id, lm.label, lm.test, lm.path, lm.file_size, lm.hash],
    ['someone/machine', 'machine', false, 'modules/machine.dkm', 900, hex(sha(mach))],
  );
  assert.deepEqual([lm.needs.ARENA, lm.needs.DSP_AREA, lm.needs.file_size], [120, 64, 900]);
  // no reader, or a file the reader says the loader would refuse: no needs, so no fit verdict (the device still refuses what does not load)
  assert.equal((await libraryModule(mach)).needs, null);
  assert.equal((await libraryModule(mach, () => null)).needs, null);
  const unknown = mergeView(
    [await libraryModule(mach, () => null)],
    [],
    [],
    { MODULES: { limit: 8, used: 8, largest: 0 } },
    {},
  );
  assert.deepEqual([unknown.modules[0].warnings, unknown.modules[0].needs], [[], undefined]);

  const library = [await libraryModule(sine, needsOf), lm, await libraryModule(tool, needsOf)];
  const card = [
    { path: 'modules/machine.dkm', size: 900, hash: hex(sha(mach)) }, // same file
    { path: 'modules/tool.dkm', size: 500, hash: 'ff' }, // same path, other bytes
    { path: 'modules/other.dkm', size: 700, hash: 'aa' }, // not from the library
    { path: 'modules/junk.dkm', size: 9, hash: 'bb' },
  ];
  const headers = { 'modules/other.dkm': dkm('someone/effect', { fx: 1, size: 700 }), 'modules/junk.dkm': null };
  // the device has 1000 bytes of DSP area, 50 of them in one free piece (a loaded module sits in the rest)
  const cap = {
    MODULES: { limit: 8, used: 1, largest: 0 },
    ARENA: { limit: 1000, used: 0, largest: 300 },
    DSP_AREA: { limit: 1000, used: 0, largest: 50 },
  };
  const v = mergeView(library, card, ['someone/machine', 'someone/effect'], cap, headers);
  assert.deepEqual(
    v.modules.map((r) => r.id),
    ['someone/machine', 'test/sine', 'someone/tool'],
  ); // group, test last, id
  const by = Object.fromEntries(v.modules.map((r) => [r.id, r]));
  assert.deepEqual(
    [by['someone/machine'].on_card, by['someone/machine'].loaded, by['someone/machine'].fits],
    ['yes', true, true],
  );
  assert.equal(by['someone/tool'].on_card, 'other version');
  assert.deepEqual([by['test/sine'].on_card, by['test/sine'].fits, by['test/sine'].test], ['no', false, true]); // DSP 400 > largest 50
  assert.match(
    by['test/sine'].warnings[0],
    /DSP memory 350 bytes short; fits when the modules loaded now are not selected/,
  );
  // more than the device has at all: no way out by deselecting
  const huge = await libraryModule(dkm('test/huge', { machine: 1, dsp: 2000, size: 800 }), needsOf);
  assert.match(
    mergeView([huge], [], [], cap, {}).modules[0].warnings[0],
    /DSP memory .* short; more than this device has/,
  );
  // a capacity without a kind (an older runtime) does not judge it
  assert.equal(mergeView(library, [], [], { MODULES: cap.MODULES }, {}).modules[1].fits, true);
  const extra = Object.fromEntries(v.card_only.map((r) => [r.path, r]));
  assert.deepEqual(
    [extra['modules/other.dkm'].group, extra['modules/other.dkm'].loaded, extra['modules/other.dkm'].name],
    ['Effects', true, 'someone/effect'],
  );
  assert.deepEqual([extra['modules/junk.dkm'].known, extra['modules/junk.dkm'].group], [false, 'Tools']);
  assert.equal(mergeView(library, [], [], null, {}).modules[0].fits, null); // no CAPACITY: no verdict
});

test('buildView reads the device: INFO, LIST, MODULES, CAPACITY and the headers of the card-only files by READ', async () => {
  const dev = new FakeDevice();
  const mach = dkm('someone/machine', { machine: 1, dsp: 64, size: 900 });
  dev.files.set('modules/machine.dkm', mach);
  dev.files.set('modules/other.dkm', dkm('someone/effect', { fx: 1, size: 700 }));
  dev.loaded = ['someone/machine'];
  const view = await buildView(session(dev), [await libraryModule(mach, needsOf)]);
  assert.deepEqual(
    [view.device, view.runtime, view.free_bytes, view.loaded],
    ['dt2', '0.2.4', 1 << 20, ['someone/machine']],
  );
  assert.equal(view.modules[0].on_card, 'yes');
  assert.deepEqual([view.card_only[0].group, view.card_only[0].version], ['Effects', '0.1.2']);
  assert.ok(dev.seen.includes('READ') && dev.seen.includes('MODULES') && dev.seen.includes('CAPACITY'));
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
  const lib = [await libraryModule(mach, needsOf)];
  const rows = (v) => [...v.modules, ...v.card_only].map((r) => r.path ?? r.id).sort();
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
  old.caps = Object.values(CAP).reduce((a, bit) => a | (1 << bit), 0) & ~(1 << CAP.FOLDERS);
  assert.deepEqual(rows(await buildView(session(old), lib)), ['modules/other.dkm', 'modules/machine.dkm'].sort());
  // install compares with the modules/ entries too, and leaves the samples alone
  const events = [];
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

test('install: skip the same file hash, upload the rest with capped progress, VERIFY each, no SELECT', async () => {
  const dev = new FakeDevice();
  const mach = dkm('someone/machine', { machine: 1, size: 900 });
  const big = dkm('test/big', { size: 20000 });
  dev.files.set('modules/machine.dkm', mach); // already on the card
  const events = [];
  const out = await install(
    session(dev, { chunk: 512 }),
    ['someone/machine', 'test/big'],
    [await libraryModule(mach, needsOf), await libraryModule(big, needsOf)],
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
    install(session(dev), ['someone/none'], [await libraryModule(mach, needsOf)]),
    /not a run-time module/,
  );
});

test('install: a VERIFY that does not match is an error', async () => {
  const dev = new FakeDevice();
  dev.corrupt = true;
  const m = dkm('someone/machine', { machine: 1 });
  const events = [];
  await assert.rejects(
    install(session(dev), ['someone/machine'], [await libraryModule(m, needsOf)], (e) => events.push(e)),
    /did not keep the file/,
  );
  assert.deepEqual(events.at(-1), { event: 'verify', id: 'someone/machine', path: 'modules/machine.dkm', ok: false });
});

test('the upload progress callback: at most a tenth apart, always the last', () => {
  const ev = [];
  const cb = uploadProgress((e) => ev.push(e), 'a/b', 700);
  cb(700, 700);
  assert.deepEqual(ev, [{ event: 'progress', id: 'a/b', sent: 700, bytes: 700 }]);
  const many = [];
  const cb2 = uploadProgress((e) => many.push(e), 'a/c', 5);
  for (let n = 1; n <= 5; n++) cb2(n, 5);
  assert.equal(many.at(-1).sent, 5);
  assert.ok(many.length <= PROGRESS_EVENTS + 1);
});

test('delete: one DELETE per path, confirmed by LIST; a path that is not on the card sends nothing', async () => {
  const dev = new FakeDevice();
  for (const n of ['a', 'b', 'c']) dev.files.set(`modules/${n}.dkm`, dkm(`x/${n}`));
  const events = [];
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

test('restart: RESTART, wait for the new boot in the log, then report what MODULES lists', async () => {
  const dev = new FakeDevice();
  dev.selection = ['modules/machine.dkm'];
  const events = [];
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
  await assert.rejects(session(new FakeDevice()).rpc(2, new Uint8Array(), 20), (e) => e.kind === 'timeout');
});
