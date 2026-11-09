import fs from 'node:fs';
import path from 'node:path';
import {
  be32,
  concat,
  frame,
  HEADER_BYTES,
  hasCap,
  parseReply,
  rpcRequest,
  splitMessages,
  u32,
  type DkSession,
} from '@digi/dk-session';
import { Session } from '@digi/test-support';
import { expect } from 'vitest';
import { blob, listing, pokeWord, sha, statusOf, type Files } from '@digi/regression/runtime/card-kit.ts';
import type { CardRig } from '@digi/regression/runtime/card-rig.ts';
import { options, word } from '@digi/regression/runtime/fx-live.ts';
import { tap } from '@digi/regression/runtime/fx-session.ts';
import { portFile } from '@digi/regression/runtime/device-data.ts';
import { liveCheck as check } from '@digi/regression/runtime/artifacts.ts';
import { FIRMWARE } from '@digi/regression/runtime/devices.ts';
import { planSymbol } from '@digi/regression/runtime/runtime-plan.ts';
import {
  beginBody,
  clearFiles,
  cstr,
  dataBody,
  deviceListing,
  GCMD,
  putWithHash,
  roundtrip,
  runtimeVersion,
  shaBytes,
  uploadTests,
  usbSend,
  usbTake,
} from '@digi/regression/runtime/upload-kit.ts';

/** Python tools/patchkit/pk_digikit_upload.py: the `quick`, `ops`, `folders`, `stock`, `txfull` and `screenshot` parts. */

const text = new TextEncoder();
const MIB = 1024 * 1024;
const RPC_HEADER = [0xf0, 0x00, 0x20, 0x3c, 0x10, 0x00];
const hasRpcHeader = (m: Uint8Array) => RPC_HEADER.every((b, i) => m[i] === b);

/** Upload `data` (a new file or a replace) and record it in `files`. */
async function put(dk: DkSession, files: Files, p: string, data: Buffer): Promise<void> {
  files[p] = data;
  await dk.put(p, data);
}

/** The device's LIST, the card read by plusdrive after the guest's writes, and the bytes sent all agree. */
async function expectCard(rig: CardRig, label: string, files: Files): Promise<void> {
  expect(await deviceListing(rig.dk), `${label}: LIST equals the files sent`).toEqual(listing(files));
  const view = await rig.view(`ops-${label.replace(/\W+/g, '-')}`);
  expect(listing(view.files), `${label}: the card, read by plusdrive, holds the same bytes ${view.head}`).toEqual(
    listing(files),
  );
  check(`${label}: store version 4`, view.head.includes('version=4'), view.head);
}

// ---- ops and quick ------------------------------------------------------------------------------------------------------------------

/** READ: the header, a range across a sector boundary, the 768-byte cap and the end of the file equal the bytes sent; a missing file, an offset past the end and length 0 are refused. */
async function readChecks(rig: CardRig, file: string, data: Buffer): Promise<void> {
  const dk = rig.dk;
  expect(Buffer.from(await dk.header(file)), 'READ: the first 0xa0 bytes equal the file sent').toEqual(
    data.subarray(0, HEADER_BYTES),
  );
  const range = await dk.read(file, 500, 700);
  check(
    'READ: a range across a sector boundary, and the file size',
    range.size === data.length && Buffer.from(range.bytes).equals(data.subarray(500, 1200)),
    `${range.size} ${range.bytes.length}`,
  );
  const long = await dk.read(file, 1, 5000);
  check(
    'READ: a long request returns 768 bytes',
    Buffer.from(long.bytes).equals(data.subarray(1, 769)),
    String(long.bytes.length),
  );
  const tail = await dk.read(file, data.length - 10, 100);
  check('READ: a range past the end is cut at the end', Buffer.from(tail.bytes).equals(data.subarray(-10)));
  const parts: Buffer[] = [];
  for (let off = 0; off < data.length; off += 768) parts.push(Buffer.from((await dk.read(file, off, 768)).bytes));
  check('READ: the whole file by 768-byte reads', Buffer.concat(parts).equals(data));
  check(
    'VERIFY: the device hash of the file on the card equals the local one',
    (await dk.verify(file)) === sha(data),
  );
  const missing = await statusOf(() => dk.verify('modules/none.dkm'));
  check('VERIFY: a missing file is refused', missing === -59, String(missing));
  for (const [what, args, status] of [
    ['a missing file', ['modules/none.dkm', 0, 4], -59],
    ['offset at the end', [file, data.length, 4], -54],
    ['length 0', [file, 0, 0], -55],
  ] as const) {
    const got = await statusOf(() => dk.read(args[0], args[1], args[2]));
    check(`READ: ${what} is refused`, got === status, String(got));
  }
}

async function ops(rig: CardRig, quick: boolean): Promise<void> {
  const dk = await rig.pausedBoot(await rig.card({ name: 'ops' }));
  await rig.applyCap();
  const files: Files = {};
  const info = await dk.info();
  check(
    "INFO: protocol 1, the runtime's version, the device, the free bytes of an empty store, 32 KiB chunks",
    info.protocol === 1 &&
      info.runtime === runtimeVersion() &&
      info.port === rig.device &&
      info.free_bytes === rig.freeFresh() &&
      info.max_chunk === 32768 &&
      !info.safe_mode &&
      !info.upload_open,
    JSON.stringify(info),
  );
  check('LIST of a card with no store: empty', (await dk.list()).length === 0);

  const a = blob(70_000, 1); // three chunks, the last partial
  files['modules/a.dkm'] = a;
  check('put a 70,000-byte file', (await statusOf(() => dk.put('modules/a.dkm', a))) === 0);
  await expectCard(rig, 'first upload', files);
  await readChecks(rig, 'modules/a.dkm', a);

  if (quick) {
    await put(dk, files, 'modules/b.dkm', blob(512, 2));
    await put(dk, files, 'modules/a.dkm', blob(600, 3));
    await expectCard(rig, 'replace', files);
    await dk.delete('modules/b.dkm');
    delete files['modules/b.dkm'];
    await expectCard(rig, 'delete', files);
  } else {
    await opsFull(rig, files);
    await roundtrip(rig, files);
  }
  const written = await rig.writes('ops-final');
  check(
    'the upload wrote no selection sector',
    !written.has(rig.store.selA) && !written.has(rig.store.selB),
    [...written].map((s) => s.toString(16)).join(' '),
  );
}

async function opsFull(rig: CardRig, files: Files): Promise<void> {
  const dk = rig.dk;
  const status = (cmd: number, body?: Uint8Array, version?: number) => rig.status(cmd, body, version);
  // sizes around a sector, a one-byte file and an empty file, a nested path, sorted insert before and after
  for (const [p, n, seed] of [
    ['a/z.bin', 513, 4],
    ['modules/b.dkm', 512, 5],
    ['modules/c.dkm', 1, 6],
    ['empty', 0, 7],
    ['modules/big.dkm', 65_536, 8],
  ] as const) {
    const data = blob(n, seed);
    files[p] = data;
    const st = await statusOf(() => dk.put(p, data));
    check(`put ${p} (${n} bytes)`, st === 0, String(st));
  }
  await expectCard(rig, 'six files', files);
  await put(dk, files, 'modules/a.dkm', blob(1000, 9));
  await expectCard(rig, 'replace', files);

  // delete, and a delete of a file that is not there
  await dk.delete('modules/c.dkm');
  delete files['modules/c.dkm'];
  const missing = await status(GCMD.DELETE, cstr('modules/c.dkm'));
  check('DELETE of a missing path: E_NOFILE', missing === -59, String(missing));
  await expectCard(rig, 'delete', files);

  // E_FULL: BEGIN past the free bytes answers with them; BEGIN of exactly the free bytes is accepted
  const free = (await dk.info()).free_bytes;
  const full = await rig.request(GCMD.BEGIN, beginBody('x', free + 1));
  check(
    'BEGIN past the free bytes: E_FULL with the free bytes',
    full.status === -52 && u32(full.body, 0) === free,
    `${full.status} free ${free}`,
  );
  // (free space is the sum of the gaps: a file as big as that is accepted only on a store with one gap, tried after CLEAR below)
  check('BEGIN of one sector is accepted', (await status(GCMD.BEGIN, beginBody('x', 512))) === 0);
  check('INFO: an upload is open', (await dk.info()).upload_open);
  await dk.abort();
  check('ABORT closes it', !(await dk.info()).upload_open);

  // E_HASH: nothing is committed
  const bad = await putWithHash(rig, 'modules/bad.dkm', blob(40_000, 10), shaBytes(text.encode('not the file')));
  check('a wrong file hash: E_HASH', bad === -56, String(bad));
  check('an upload ended by END is closed', !(await dk.info()).upload_open);
  await expectCard(rig, 'after E_HASH (nothing committed)', files);

  // DATA: retry, bad offset, bad length, no upload, too much
  const d = blob(2048, 11);
  const first = d.subarray(0, 1024);
  await rig.send(GCMD.BEGIN, beginBody('retry.bin', d.length, shaBytes(d)));
  const sent = [await status(GCMD.DATA, dataBody(0, first)), await status(GCMD.DATA, dataBody(0, first))];
  check('a DATA retry (same offset, same length) is ok', sent[0] === 0 && sent[1] === 0, String(sent));
  const dataCases: [string, Uint8Array, number[]][] = [
    ['a DATA at an offset that is neither next nor a retry: E_OFFSET', dataBody(512, d.subarray(512, 1024)), [-54]],
    ['a DATA that is not a multiple of 512 and not the last: E_LENGTH', dataBody(1024, d.subarray(1024, 1500)), [-55]],
    ["a DATA past the file's size: E_LENGTH", dataBody(1024, concat(d.subarray(1024), text.encode('x'))), [-55]],
    ['a DATA with no bytes: E_MSG or E_LENGTH', dataBody(1024, new Uint8Array()), [-50, -55]],
  ];
  for (const [what, body, want] of dataCases) {
    const st = await status(GCMD.DATA, body);
    check(what, want.includes(st), String(st));
  }
  const early = await status(GCMD.END);
  check('END before all bytes: E_LENGTH', early === -55, String(early));
  await rig.send(GCMD.BEGIN, beginBody('retry.bin', d.length, shaBytes(d)));
  await rig.send(GCMD.DATA, dataBody(0, first));
  await rig.send(GCMD.DATA, dataBody(0, first));
  check('the rest of the file after the retry', (await status(GCMD.DATA, dataBody(1024, d.subarray(1024)))) === 0);
  await rig.send(GCMD.END);
  files['retry.bin'] = d;
  await expectCard(rig, 'after the retry', files);
  const noUpload = [await status(GCMD.DATA, dataBody(0, d.subarray(0, 512))), await status(GCMD.END)];
  check(
    'DATA and END with no upload open: E_STATE',
    noUpload.every((st) => st === -53),
    String(noUpload),
  );

  // paths and messages
  for (const p of ['', '/a', 'a//b', 'a/', 'a/../b', './a', 'x'.repeat(160)]) {
    const st = await status(GCMD.BEGIN, beginBody(p, 1));
    check(`BEGIN with the path ${JSON.stringify(p.slice(0, 12))}: E_PATH`, st === -51, String(st));
  }
  const protocolCases: [string, number, number, Uint8Array?, number?][] = [
    ['a short BEGIN: E_MSG', -50, GCMD.BEGIN, concat(text.encode('x'), Uint8Array.of(0, 0))],
    ['CLEAR without its key: E_MSG', -50, GCMD.CLEAR, be32(1)],
    ['another protocol version: E_PROTO', -60, GCMD.INFO, new Uint8Array(), 2],
    ['an unknown command: E_PROTO', -60, 0x33],
  ];
  for (const [what, want, cmd, body, version] of protocolCases) {
    const st = await status(cmd, body, version);
    check(what, st === want, String(st));
  }
  expect(await deviceListing(dk), 'nothing above changed the store').toEqual(listing(files));

  // CLEAR
  await dk.clear();
  clearFiles(files);
  await expectCard(rig, 'CLEAR', files);
  const cleared = await dk.info();
  check('CLEAR gives the space back', cleared.free_bytes === rig.freeFresh(), JSON.stringify(cleared));
  // one gap: a file of exactly the free bytes is accepted (the table goes beside it)
  check(
    'BEGIN of exactly the free bytes is accepted',
    (await status(GCMD.BEGIN, beginBody('x', cleared.free_bytes))) === 0,
  );
  await dk.abort();
  await put(dk, files, 'again.dkm', blob(5000, 12));
  await expectCard(rig, 'an upload after CLEAR', files);
}

uploadTests('upload-quick: one upload, a replace and a delete', (rig) => ops(rig, true), {
  devices: ['dt2'],
  symbols: ['DKRT_STORE_CAP'],
});
uploadTests('upload-ops: the commands, errors and CLEAR', (rig) => ops(rig, false), {
  full: true,
  symbols: ['DKRT_STORE_CAP'],
});

// ---- folders and large files --------------------------------------------------------------------------------------------------------

/** Folders, LIST by prefix, MKDIR, CAPACITY's STORE entry, a streamed upload of several MiB, a delete that frees its space for the next file, and LIST paging. */
async function folders(rig: CardRig): Promise<void> {
  const mib = 6;
  const dk = await rig.pausedBoot(await rig.card({ name: 'folders' }));
  await rig.applyCap();
  const files: Files = {};
  const zeroHash = '0'.repeat(64);
  const status = (cmd: number, body?: Uint8Array) => rig.status(cmd, body);
  check('INFO: capability FOLDERS', hasCap(await dk.info(), 'FOLDERS'));
  const totalKib = Math.floor(rig.store.limit / 2);
  const cap = (await dk.capacity()).STORE;
  check(
    "CAPACITY: a STORE entry with the area's size in KiB, nothing used yet, largest 0",
    cap !== undefined && cap.limit === totalKib && cap.used === 0 && cap.largest === 0,
    JSON.stringify(cap),
  );

  let step = 0;
  /** LIST and the card after the guest's writes list the files sent and the folders with nothing in them; returns the first sectors. */
  const verify = async (label: string, dirs: string[]): Promise<Record<string, number>> => {
    const store = await rig.ls(`fold-${++step}`);
    const listed = await dk.list();
    const isFolder = (e: { size: number; hash: string }) => e.size === 0 && e.hash === zeroHash;
    expect(
      Object.fromEntries(listed.filter((e) => !isFolder(e)).map((e) => [e.path, [e.size, e.hash]])),
      `${label}: LIST equals the files sent`,
    ).toEqual(listing(files));
    const onCard = store.entries.filter((e) => !e.dir);
    expect(
      Object.fromEntries(onCard.map((e) => [e.path, [e.size, e.hash]])),
      `${label}: the card, read by plusdrive, lists the same files and sizes ${store.head}`,
    ).toEqual(listing(files));
    const folderNames = listed.filter((e) => isFolder(e) && !(e.path in files)).map((e) => e.path);
    expect(folderNames.sort(), `${label}: folders with nothing in them: LIST = the card`).toEqual([...dirs].sort());
    expect(
      store.entries
        .filter((e) => e.dir)
        .map((e) => e.path)
        .sort(),
    ).toEqual([...dirs].sort());
    return Object.fromEntries(onCard.map((e) => [e.path, e.firstSector]));
  };

  check('LIST under samples/ of an empty store: nothing', (await dk.list('samples/')).length === 0);
  await dk.mkdir('samples');
  await dk.mkdir('samples/drums');
  await dk.mkdir('samples/drums'); // exists: ok
  await verify('two MKDIR (the outer folder has a folder in it: one entry)', ['samples/drums']);
  for (const bad of ['a//b', '../x', 'a/']) {
    const st = await status(GCMD.MKDIR, cstr(bad));
    check(`MKDIR ${JSON.stringify(bad)}: E_PATH`, st === -51, String(st));
  }
  const emptyPath = await status(GCMD.MKDIR, cstr(''));
  check('MKDIR with an empty path: E_MSG', emptyPath === -50, String(emptyPath));
  const noNul = await status(GCMD.MKDIR, text.encode('x'));
  check('MKDIR with no NUL: E_MSG or E_PATH', noNul === -50 || noNul === -51, String(noNul));
  const prefixNoNul = await status(GCMD.LIST, concat(be32(0), text.encode('sam')));
  check('LIST with a prefix and no NUL: E_MSG', prefixNoNul === -50, String(prefixNoNul));

  // a streamed upload: the file is several MiB, far over the 64 KiB staging buffer, and goes to the card as it arrives
  const free0 = (await dk.info()).free_bytes;
  const big = blob(mib * MIB + 77, 21);
  const bigPath = 'samples/drums/big.wav';
  files[bigPath] = big;
  const putSt = await statusOf(() => dk.put(bigPath, big));
  check(
    `put a ${mib} MiB file under a folder entry (DATA chunks of 32 KiB straight to the card)`,
    putSt === 0,
    String(putSt),
  );
  const free1 = (await dk.info()).free_bytes;
  check(
    "INFO: the free bytes fell by the file's sectors (and the table room)",
    free0 - free1 > 0 && free0 - free1 <= big.length + 2 * 512 + 512,
    `${free0} -> ${free1}`,
  );
  const cap1 = (await dk.capacity()).STORE!;
  check(
    'CAPACITY: STORE used grew by the file in KiB',
    cap1.limit === totalKib && mib * 1024 <= cap1.used && cap1.used <= mib * 1024 + 8,
    JSON.stringify(cap1),
  );
  let rows = await verify('big upload (the folder entry is gone)', []);
  check(
    `VERIFY by ranges: the device hashes the ${mib} MiB file from the card in 1 MiB pieces, each equal to the host's`,
    await dk.verifyFile(bigPath, big),
  );
  const lastStart = Math.floor(big.length / 512) * 512;
  check(
    'VERIFY of the last, short range (not a whole sector)',
    (await dk.verify(bigPath, lastStart, big.length % 512)) === sha(big.subarray(lastStart)),
  );
  for (const [off, ln, want] of [
    [100, 10, -54],
    [big.length + 512, 1, -54],
    [512, 0, -55],
    [512, big.length, -55],
  ] as const) {
    const st = await status(GCMD.VERIFY, concat(cstr(bigPath), be32(off), be32(ln)));
    check(`VERIFY range (${off}, ${ln}): refused`, st === want, String(st));
  }
  const bigFirst = rows[bigPath]!;

  // a delete frees the space and the next file of the size takes it again (first fit)
  await dk.delete(bigPath);
  delete files[bigPath];
  await verify('delete', []);
  const freeBack = (await dk.info()).free_bytes;
  check(
    'the delete gave the free bytes back (the folder was implied: no entry is left)',
    free0 <= freeBack && freeBack <= free0 + 1024, // the empty table needs no sector
    `${freeBack} vs ${free0}`,
  );
  const cap2 = (await dk.capacity()).STORE!;
  check('CAPACITY: STORE used fell again', cap2.used < 16, JSON.stringify(cap2));
  await put(dk, files, 'samples/second.wav', blob(mib * MIB + 77, 22));
  rows = await verify('a file of the same size', []);
  check(
    'the new file took the first gap, which holds the space the deleted one left (it starts at or before it)',
    rows['samples/second.wav']! <= bigFirst,
    `${rows['samples/second.wav']} vs ${bigFirst}`,
  );

  // a hole in the middle: three files, delete the middle one, a smaller file goes into the hole
  for (let i = 0; i < 3; i++) await put(dk, files, `holes/f${i}.bin`, blob(100_000, 30 + i));
  rows = await verify('three files in a row', []);
  const mid = rows['holes/f1.bin']!;
  await dk.delete('holes/f1.bin');
  delete files['holes/f1.bin'];
  await put(dk, files, 'holes/small.bin', blob(50_000, 33));
  rows = await verify('a smaller file after a delete in the middle', []);
  const small = rows['holes/small.bin']!;
  check(
    'the smaller file went into the hole between f0 and f2',
    rows['holes/f0.bin']! < small && small + 98 <= rows['holes/f2.bin']! && small <= mid,
    `${small} (f0 ${rows['holes/f0.bin']}, hole ${mid}, f2 ${rows['holes/f2.bin']})`,
  );

  // MKDIR against files, implied folders and folder entries; DELETE of folders
  for (const bad of ['holes/f0.bin', 'holes/f0.bin/x']) {
    const st = await status(GCMD.MKDIR, cstr(bad));
    check(`MKDIR ${JSON.stringify(bad)} (a file, or inside one): E_PATH`, st === -51, String(st));
  }
  await dk.mkdir('holes'); // implied by the files in it: ok, no entry
  await verify('MKDIR of an implied folder makes no entry', []);
  await dk.mkdir('tmp/x');
  await verify('MKDIR tmp/x', ['tmp/x']);
  const overFolder = await status(GCMD.BEGIN, beginBody('tmp/x', 1));
  check('BEGIN over a folder entry: E_PATH', overFolder === -51, String(overFolder));
  const impliedDelete = await status(GCMD.DELETE, cstr('holes'));
  check('DELETE of an implied folder: E_NOFILE (delete its files)', impliedDelete === -59, String(impliedDelete));
  await dk.delete('tmp/x');
  await verify('DELETE of a folder entry', []);

  // RENAME: a file moves with its sectors and its hash (no data is copied), the folder it leaves stays, and an empty folder renames
  check('INFO: capability RENAME', hasCap(await dk.info(), 'RENAME'));
  const move = async (from: string, to: string) => {
    await dk.rename(from, to);
    files[to] = files[from]!;
    delete files[from];
  };
  await move('holes/small.bin', 'moved/small.bin');
  rows = await verify('RENAME into a new folder (holes keeps f0 and f2: no entry)', []);
  check(
    'RENAME keeps the file sectors: the moved file starts where it was',
    rows['moved/small.bin'] === small,
    `${rows['moved/small.bin']} vs ${small}`,
  );
  await move('holes/f0.bin', 'holes/F0 renamed.bin');
  await verify('RENAME in the same folder', []);
  await move('holes/f2.bin', 'moved/f2.bin');
  await move('holes/F0 renamed.bin', 'moved/f0.bin');
  await verify('the folder the last file left stays, as a folder entry', ['holes']);
  for (const [from, to, want] of [
    ['moved/nope', 'moved/x', -59],
    ['moved/f0.bin', 'moved/f2.bin', -51],
    ['moved/f0.bin', 'moved', -51],
    ['moved/f0.bin', 'moved/f0.bin/x', -51],
    ['holes', 'holes/in', -51],
    ['moved', 'elsewhere', -59],
    ['moved/f0.bin', 'a//b', -51],
  ] as const) {
    const st = await status(GCMD.RENAME, concat(cstr(from), cstr(to)));
    check(`RENAME ${JSON.stringify(from)} -> ${JSON.stringify(to)}: refused (${want})`, st === want, String(st));
  }
  const oneArg = await status(GCMD.RENAME, cstr('moved/f0.bin'));
  check('RENAME with no second path: E_MSG or E_PATH', oneArg === -50 || oneArg === -51, String(oneArg));
  await verify('refused renames change nothing', ['holes']);
  await dk.rename('holes', 'empty');
  await verify('RENAME of an empty folder', ['empty']);
  await dk.delete('empty');
  await verify('DELETE of the renamed folder', []);

  // LIST paging: many entries, several replies
  for (let i = 0; i < 40; i++) {
    await put(dk, files, `many/s${String(i).padStart(2, '0')}.wav`, blob(1 + i, 100 + i));
  }
  await verify('forty small files', []);
  const many = await dk.list('many/');
  check(
    'LIST under many/: forty entries in several pages',
    many.length === 40 &&
      many
        .map((e) => e.path)
        .sort()
        .join() ===
        Object.keys(files)
          .filter((p) => p.startsWith('many/'))
          .sort()
          .join(),
    String(many.length),
  );
  check('LIST under a prefix nobody has: nothing', (await dk.list('zzz/')).length === 0);
  check(
    'LIST with the prefix `m` finds many/ and nothing else',
    [...new Set((await dk.list('m')).map((e) => e.path.split('/')[0]))].join() === 'many',
  );
  await roundtrip(rig, files);
}

uploadTests('upload-folders: folders, ranges, a streamed upload, first fit and paging', folders, {
  full: true,
  symbols: ['DKRT_STORE_CAP'],
  stallMs: 10_000, // the END and the ranged VERIFY hash MiBs in one request: the main loop does not beat for over a second
});

// ---- the stock route ----------------------------------------------------------------------------------------------------------------

/** Talk to a paused daemon: the stock requests and a message that starts F0 7D but is not ours; the replies in order. */
async function talk(session: Session): Promise<Uint8Array[]> {
  await usbTake(session); // start the capture
  const requests = [1, 2, 3].map((type) => rpcRequest(type, 1));
  // a message that starts F0 7D but is not ours (no 64 6B): the router ignores it; the Ping after it still answers
  const foreign = Uint8Array.of(0xf0, 0x7d, 0x01, 0x02, 0x03, 0xf7);
  const got: Uint8Array[] = [];
  let buf: Uint8Array = new Uint8Array();
  for (const request of [...requests.slice(0, 2), foreign, ...requests]) {
    await usbSend(session, request);
    const { messages, rest } = splitMessages(concat(buf, await usbTake(session)));
    got.push(...messages);
    buf = rest;
  }
  return got;
}

uploadTests(
  'upload-stock: MidiRpc Ping, SoftwareVersion and DeviceUID are the stock replies',
  async (rig) => {
    await rig.pausedBoot(await rig.card({ name: 'stock' }));
    const runtime = await talk(rig.session);
    // the reference is the unpatched firmware booted from its snapshot (no runtime, no card image)
    const stockSession = await Session.start({ ...options(rig.device), dsp: 'zero' });
    let stock: Uint8Array[];
    try {
      await stockSession.wait(300);
      stock = await talk(stockSession);
    } finally {
      await stockSession.close();
    }
    check(
      'the stock firmware answered the five MidiRpc requests',
      stock.length === 5 && stock.every(hasRpcHeader),
      stock.map((m) => Buffer.from(m.subarray(0, 8)).toString('hex')).join(' '),
    );
    expect(
      runtime.map((m) => Buffer.from(m).toString('hex')),
      'Ping, SoftwareVersion and DeviceUID replies are byte-identical with and without the runtime',
    ).toEqual(stock.map((m) => Buffer.from(m).toString('hex')));
  },
  { full: true },
);

// ---- a host that never reads --------------------------------------------------------------------------------------------------------

/** A host that never reads its MIDI input leaves the firmware's TX ring full. The runtime checks the bytes in flight before it sends and drops the reply; the emulator does not model the USB IN endpoint, so the test sets the in-flight word of the TX function. */
uploadTests(
  'upload-txfull: replies are dropped while the TX ring is full',
  async (rig) => {
    const { session, device } = rig;
    const dk = await rig.pausedBoot(await rig.card({ name: 'txfull' }));
    const symbol = (name: string) => planSymbol(rig.plan, name);
    // the 4-byte address literal of the code anchor (not word aligned: read as bytes)
    const anchor = portFile(device, FIRMWARE[device]).anchor?.find((a) => a.name === 'cf.midi.site.tx_inflight_lit');
    if (anchor?.addr === undefined) throw new Error('no anchor cf.midi.site.tx_inflight_lit in the port file');
    const at = anchor.addr & ~3;
    const literal = Buffer.alloc(8);
    literal.writeUInt32BE(await word(session, at), 0);
    literal.writeUInt32BE(await word(session, at + 4), 4);
    const flight = literal.readUInt32BE(anchor.addr - at);
    check(
      'the in-flight word is a DDR address',
      flight >= 0x40000000 && flight < 0x48000000 && flight % 4 === 0,
      flight.toString(16),
    );
    check('an INFO answers while the ring is empty', (await dk.info()).port === device);
    const dropped0 = await word(session, symbol('DKRT_UPLOAD_DROPPED'));
    const sent0 = await word(session, symbol('DKRT_UPLOAD_COUNT'));

    const raw = async (msg: Uint8Array) => {
      await usbTake(session); // empty the capture
      await usbSend(session, msg);
      return usbTake(session);
    };
    const request = frame(GCMD.INFO, 5);
    for (const [level, name] of [
      [0x0f00, 'a full ring'],
      [0x0200, '512 bytes in flight'],
    ] as const) {
      await pokeWord(session, flight, level);
      const out: Uint8Array[] = [];
      for (let i = 0; i < 20; i++) out.push(await raw(request));
      check(
        `${name}: 20 requests, no reply reaches the output`,
        out.every((o) => o.length === 0),
        out.map((o) => o.length).join(),
      );
    }
    const dropped = (await word(session, symbol('DKRT_UPLOAD_DROPPED'))) - dropped0;
    check(
      'the 40 replies were dropped and counted',
      dropped === 40 && (await word(session, symbol('DKRT_UPLOAD_COUNT'))) === sent0,
      `dropped ${dropped}`,
    );
    // the MIDI task is alive: a stock Ping goes the stock way and answers
    await pokeWord(session, flight, 0x0f00);
    const ping = await raw(rpcRequest(1, 1));
    check(
      'the MIDI task still routes stock messages: the Ping answers while the ring is full',
      hasRpcHeader(ping),
      Buffer.from(ping.subarray(0, 12)).toString('hex'),
    );
    // room again: the host's retry gets its reply
    for (const [level, name] of [
      [0x0100, '256 bytes in flight'],
      [0, 'an empty ring'],
    ] as const) {
      await pokeWord(session, flight, level);
      const out = await raw(request);
      const first = splitMessages(out).messages[0];
      check(
        `${name}: the reply is sent`,
        first !== undefined && parseReply(first) !== null,
        Buffer.from(out).toString('hex'),
      );
    }
  },
  { full: true, symbols: ['DKRT_UPLOAD_DROPPED', 'DKRT_UPLOAD_COUNT'] },
);

// ---- the firmware's own Screenshot ----------------------------------------------------------------------------------------------------

/** The firmware's own MidiRpc Screenshot (0x04) over the emulated USB MIDI path equals the emulator's frame capture, on the main screen and on SETTINGS. */
uploadTests('upload-screenshot: the MidiRpc Screenshot equals the frame capture', async (rig) => {
  const { session } = rig;
  const dk = await rig.pausedBoot(await rig.card({ name: 'screen' }));
  const oled = async () => {
    const frame = await session.client.framebufferGet({ hash: true, data: true });
    return Buffer.from(frame.data_b64 ?? '', 'base64');
  };
  const grab = async (name: string) => {
    const before = await oled();
    const shot = Buffer.from(await dk.screen());
    const after = await oled();
    fs.writeFileSync(path.join(rig.artifacts, `${name}.oled`), shot);
    check(
      `Screenshot reply (${name}): 1024 bytes equal to the emulator's frame capture`,
      shot.length === 1024 && (shot.equals(before) || shot.equals(after)),
      `${shot.filter((b, i) => b !== after[i]).length} bytes differ from the frame after`,
    );
    check(
      `Screenshot reply (${name}): the screen is not blank`,
      shot.some((b) => b !== 0),
    );
  };
  await grab('main');
  await tap(session, 'SETTINGS', 600);
  await grab('after-settings-key');
});
