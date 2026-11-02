import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { Sample, prepareUpload } from '@digi/dk-session/dksample';
import { openSamples } from '@digi/dk-session/samples';
import { RpcFiles } from '@digi/dk-session/rpcfiles';
import { repoRoot } from '@digi/test-support';
import { expect } from 'vitest';
import type { PatchedSession } from '../support.ts';
import { liveArtifacts, record } from './artifacts.ts';
import { installModule, moduleArtifact } from './install.ts';
import { memory, panel } from './panel.ts';
import { runtimeTests } from './ready.ts';
import { settleUi } from './waits.ts';

// DT2 1.17 stock sample pool (findings 81, 82, 85). All firmware state is read with peeks.
const POOL = { slotMap: 0x405b5368, slotKey: 0x405b1368, slotLength: 0x405ba768, blocks: 0x405a0f64 };
const DSP_SLOTS = 0x257810;
const PCM_BASE = 0x8422b9c8;
const OUR_SLOTS = [0x3ff, 0x3fe];
const PROJECT_SLOT0 = 0x58000;
const PROJECT_SLOT_SECTORS = 0x8000;
const SECTOR = 512;

/** Deterministic mono 48 kHz PCM16 file; the pool keeps these bytes as they are. */
function proofWav(frames = 4096): { wav: Uint8Array; pcm: Buffer } {
  const pcm = Buffer.alloc(frames * 2);
  for (let n = 0; n < frames; n++) pcm.writeInt16LE((((n * 71) % 101) - 50) * 256, n * 2);
  const wav = Buffer.alloc(44 + pcm.length);
  wav.write('RIFF');
  wav.writeUInt32LE(wav.length - 8, 4);
  wav.write('WAVEfmt ', 8);
  wav.writeUInt32LE(16, 16);
  wav.writeUInt16LE(1, 20);
  wav.writeUInt16LE(1, 22);
  wav.writeUInt32LE(48000, 24);
  wav.writeUInt32LE(96000, 28);
  wav.writeUInt16LE(2, 32);
  wav.writeUInt16LE(16, 34);
  wav.write('data', 36);
  wav.writeUInt32LE(pcm.length, 40);
  pcm.copy(wav, 44);
  return { wav, pcm };
}

const be32 = (v: number) => {
  const b = Buffer.alloc(4);
  b.writeUInt32BE(v >>> 0);
  return b;
};

async function pool({ session }: PatchedSession) {
  const { cf, words, dsp } = memory(session);
  async function vector() {
    const begin = await cf(POOL.blocks);
    const end = await cf(POOL.blocks + 4);
    return { begin, records: (end - begin) / 0x34 };
  }
  /** Map index, key, block record fields, DSP slot record and the first 64 PCM bytes of one slot. */
  async function slot(n: number) {
    const index = (await cf(POOL.slotMap + n * 4)) | 0;
    const { begin } = await vector();
    const block = index >= 0 ? await words(begin + index * 0x34, 13) : null;
    const header = await dsp(DSP_SLOTS + n * 0x14, 20);
    expect(header.unknown).toBe(0);
    const raw = Buffer.from(header.hex, 'hex');
    const length = raw.readUInt32LE(8);
    const head = length === 0 ? null : await dsp((PCM_BASE + raw.readUInt32LE(0)) >>> 0, 64);
    return {
      index,
      mapWord: await cf(POOL.slotMap + n * 4),
      lengthWord: await cf(POOL.slotLength + n * 4),
      key: await words(POOL.slotKey + n * 16, 4),
      offset: block?.[4] ?? null,
      size: block?.[5] ?? null,
      sharers: block?.[12] ?? null,
      dspRecord: header.hex,
      dspLength: length,
      pcmHead: head?.hex ?? null,
    };
  }
  return { cf, words, vector, slot };
}

/** The module's commands (tests/fixtures/modules/stock-pool). */
function moduleApi({ dk }: PatchedSession) {
  async function request(cmd: number, body: Buffer) {
    const reply = await dk.request(cmd, body);
    expect(reply.status, `stock-pool command 0x${cmd.toString(16)}`).toBe(0);
    return Buffer.from(reply.body);
  }
  async function status() {
    const b = await request(0x39, Buffer.alloc(0));
    return {
      state: b.readUInt32BE(0),
      loadRc: b.readInt32BE(4),
      openRc: b.readInt32BE(8),
      keyRc: b.readInt32BE(12),
      commandTask: b.readUInt32BE(16),
      jobTask: b.readUInt32BE(20),
      workerTask: b.readUInt32BE(24),
      key: [32, 36, 40, 44].map((o) => b.readUInt32BE(o - 4)),
      jobs: b.readUInt32BE(44),
    };
  }
  /** mode 0: the loader runs in the command task; 1: on the sample loader worker; 2: the stock assign wrapper (project entry). */
  async function load(slot: number, mode: 0 | 1 | 2, source: { path: string } | { key: number[] }) {
    const tail =
      'path' in source
        ? Buffer.concat([Buffer.from([0]), Buffer.from(`${source.path}\0`)])
        : Buffer.concat([Buffer.from([1]), ...source.key.map(be32)]);
    await request(0x38, Buffer.concat([Buffer.from([mode]), be32(slot), tail]));
    await expect.poll(async () => (await status()).state, { timeout: 30_000, interval: 50 }).toBe(2);
    return status();
  }
  async function reload(reset: number) {
    const b = await request(0x3a, Buffer.from([reset]));
    return { commandTask: b.readUInt32BE(0), workerTask: b.readUInt32BE(4) };
  }
  async function purge(flag: number) {
    const b = await request(0x3b, Buffer.from([flag]));
    return { result: b.readInt32BE(0), commandTask: b.readUInt32BE(4) };
  }
  return { status, load, reload, purge };
}

/** The sectors the guest wrote in the project slot area since the card was mounted (card.overlay records). */
async function projectSectors({ session }: PatchedSession, file: string) {
  await session.client.call('card.overlay', { path: file });
  const blob = readFileSync(file);
  const RECORD = 8 + 64 + SECTOR;
  expect(blob.length % RECORD).toBe(0);
  const out = new Map<number, Buffer>();
  for (let at = 0; at < blob.length; at += RECORD) {
    const sector = Number(blob.readBigUInt64LE(at));
    if (sector < PROJECT_SLOT0 || sector >= PROJECT_SLOT0 + 128 * PROJECT_SLOT_SECTORS) continue;
    const data = Buffer.alloc(SECTOR);
    for (let i = 0; i < SECTOR; i++) if (blob[at + 8 + (i >> 3)]! & (1 << (i & 7))) data[i] = blob[at + 72 + i]!;
    out.set(sector, data);
  }
  return out;
}

/** Byte differences of two sector maps, as offsets from the start of the project slot area. */
function sectorDiff(a: Map<number, Buffer>, b: Map<number, Buffer>) {
  const diffs: { offset: number; a: number | null; b: number | null }[] = [];
  for (const sector of [...new Set([...a.keys(), ...b.keys()])].sort((x, y) => x - y)) {
    const x = a.get(sector);
    const y = b.get(sector);
    for (let i = 0; i < SECTOR; i++) {
      if (x?.[i] !== y?.[i])
        diffs.push({ offset: (sector - PROJECT_SLOT0) * SECTOR + i, a: x?.[i] ?? null, b: y?.[i] ?? null });
    }
  }
  return diffs;
}

runtimeTests(
  'a module borrows the stock sample pool: dedup, file load, project save, reload and purge',
  async (runtime) => {
    const { session, dk } = runtime;
    const dir = liveArtifacts(session, 'stock-pool-borrow');
    const out = path.join(repoRoot(), 'out/runs/stock-pool-borrow');
    mkdirSync(out, { recursive: true });
    await settleUi(session);
    await installModule(dk, 'test/stock-pool', moduleArtifact('dt2', 'test/stock-pool'));
    await settleUi(session);
    const ui = panel(session);
    const mod = moduleApi(runtime);
    const p = await pool(runtime);

    // A file that is not in the project.
    const sample = await Sample.create(
      Uint8Array.from(readFileSync(path.join(repoRoot(), 'apps/web/public/dksample.wasm'))),
    );
    const backend = openSamples('stock-rpc', { session: dk, hashOf: (data) => sample.hash(data) });
    const fixture = proofWav();
    await backend.mkdir('stockpool');
    const prepared = prepareUpload(sample, 'p.wav', fixture.wav);
    await backend.put('stockpool', prepared);
    expect((await backend.verify('stockpool', prepared)).ok).toBe(true);
    const listing = await new RpcFiles(dk).readDir('/stockpool');
    record(out, 'listing', listing);

    // Stock slots loaded from the card project.
    const stock: { slot: number; state: Awaited<ReturnType<typeof p.slot>> }[] = [];
    for (let n = 1; n < 0x80; n++) {
      const state = await p.slot(n);
      if (state.index >= 0) stock.push({ slot: n, state });
    }
    record(out, 'stock-slots', stock);
    expect(stock.length, 'the card project loads samples').toBeGreaterThan(0);
    const donor = stock[0]!;
    // A slot the boot never touched reads map word 0 (zeroed BSS), not -1: only the DSP record tells it is empty.
    const untouched = [await p.slot(0x3ff), await p.slot(0x3fe)];
    record(out, 'our-slots-initial', untouched);
    for (const state of untouched) expect(state.dspLength, 'our slot starts empty').toBe(0);

    // Project saves before our loads: the first save sets one header word, the second must equal it but for that.
    await ui.tap('NO');
    async function save(name: string) {
      await ui.chord('FUNC', 'SETTINGS');
      await ui.tap('YES'); // slot 001
      await ui.settle(200);
      record(out, `${name}-frame`, await session.client.framebufferGet({ hash: true, data: true }));
      await ui.settle(3300);
      return projectSectors(runtime, path.join(dir, `${name}.overlay`));
    }
    const save1 = await save('save1');
    const save2 = await save('save2');
    const baseline = sectorDiff(save1, save2);
    record(out, 'save-baseline', { sectors: [save1.size, save2.size], diffs: baseline });
    expect(save1.size, 'the save wrote the project slot').toBeGreaterThan(0);

    // Case 1: the key of a sample the stock project holds, from the command task (dedup).
    const before = { donor: await p.slot(donor.slot), vector: await p.vector() };
    const dedup = await mod.load(0x3ff, 0, { key: donor.state.key });
    const dedupSlot = await p.slot(0x3ff);
    const after = { donor: await p.slot(donor.slot), vector: await p.vector() };
    record(out, 'case1-dedup', { donor: donor.slot, before, dedup, dedupSlot, after });
    expect(dedup.loadRc, 'loader accepts the key').toBe(0);
    expect(dedupSlot.index, 'slot 0x3ff maps to the donor record').toBe(before.donor.index);
    expect(dedupSlot.key, 'slot 0x3ff holds the donor key').toEqual(donor.state.key);
    expect(dedupSlot.dspLength, 'DSP slot record is written').toBeGreaterThan(0);
    expect(dedupSlot.pcmHead).toBe(before.donor.pcmHead);
    expect(after.vector.records, 'no new record').toBe(before.vector.records);
    expect(after.donor.sharers, 'one more sharer').toBe((before.donor.sharers ?? 0) + 1);

    // The -1 return: a slot that is a multiple of 0x80 is refused before anything changes (pool full has the same return).
    const refused = await mod.load(0x100, 0, { key: donor.state.key });
    const afterRefusal = await p.slot(donor.slot);
    record(out, 'case1b-refused', { refused, afterRefusal });
    expect(refused.loadRc, 'loader refuses slot 0x100').toBe(-1);
    expect(afterRefusal.sharers, 'no reference was added').toBe(after.donor.sharers);

    // Case 2: a file not in the project, loaded on the worker. Try the path spellings until one opens.
    const candidates = ['/stockpool/p', 'stockpool/p', '/stockpool/p.wav', '/+Drive/stockpool/p'];
    let fileLoad: (Awaited<ReturnType<typeof mod.load>> & { path: string }) | undefined;
    const tried: { path: string; openRc: number }[] = [];
    for (const candidate of candidates) {
      const result = await mod.load(0x3fe, 1, { path: candidate });
      tried.push({ path: candidate, openRc: result.openRc });
      if (result.openRc === 0) {
        fileLoad = { ...result, path: candidate };
        break;
      }
    }
    record(out, 'case2-path-candidates', tried);
    expect(fileLoad, `a path opens: ${JSON.stringify(tried)}`).toBeDefined();
    const fileSlot = await p.slot(0x3fe);
    record(out, 'case2-file', { fileLoad, fileSlot, expectedHead: fixture.pcm.subarray(0, 64).toString('hex') });
    expect(fileLoad!.loadRc).toBe(0);
    expect(fileLoad!.jobTask, 'job ran on the worker').toBe(fileLoad!.workerTask);
    expect(fileLoad!.commandTask, 'command task is not the worker').not.toBe(fileLoad!.workerTask);
    expect(fileSlot.index, 'record index of a real block').toBeGreaterThanOrEqual(0);
    expect(fileSlot.dspLength).toBe(fixture.pcm.length);
    expect(fileSlot.pcmHead).toBe(fixture.pcm.subarray(0, 64).toString('hex'));

    // B2: project save with our slots loaded.
    const save3 = await save('save3');
    const withOurs = sectorDiff(save2, save3);
    record(out, 'save-with-ours', { sectors: [save2.size, save3.size], diffs: withOurs });
    expect(withOurs, 'project slot bytes equal the save without our loads').toEqual([]);
    for (const n of OUR_SLOTS) expect((await p.slot(n)).dspLength, 'our slot survives the save').toBeGreaterThan(0);

    // B4: the stock purge of unused samples, from the command task. Control: the stock assign wrapper gives the
    // file project entries in slots no pattern should use; the purge must unload them and keep our slots.
    const entrySlots = [0x81, 0x90, 0x101, 0x201, 0x3f0]; // never a multiple of 0x80: the loader refuses those
    for (const entry of entrySlots) {
      const assigned = await mod.load(entry, 2, { path: fileLoad!.path });
      expect(assigned.loadRc, `stock wrapper assigns slot 0x${entry.toString(16)}`).toBe(0);
    }
    const assignedStates = [];
    for (const entry of entrySlots) assignedStates.push(await p.slot(entry));
    record(out, 'purge-control-assign', assignedStates);
    for (const state of assignedStates) {
      expect(state.index, 'the assigned slot shares the file block').toBe(fileSlot.index);
      expect(state.pcmHead).toBe(fileSlot.pcmHead);
    }
    const purged = await mod.purge(0);
    const purgeTrace: { t: number; states: number[] }[] = [];
    const purgeStart = Date.now();
    // The purge queues a reload on the worker; watch five seconds for any change of our slots.
    while (Date.now() - purgeStart < 5000) {
      const states = [];
      for (const n of [...entrySlots, ...OUR_SLOTS]) states.push((await p.slot(n)).dspLength);
      purgeTrace.push({ t: Date.now() - purgeStart, states });
      await new Promise((r) => setTimeout(r, 200));
    }
    const afterPurge = [];
    for (const n of [...entrySlots, ...OUR_SLOTS]) afterPurge.push(await p.slot(n));
    record(out, 'purge', { purged, purgeTrace, afterPurge });
    const ours = afterPurge.slice(entrySlots.length);
    for (const state of ours) expect(state.dspLength, 'our slot survives the purge').toBeGreaterThan(0);
    expect(
      purgeTrace.every((t) => t.states.slice(entrySlots.length).every((v) => v > 0)),
      'our slots never emptied',
    ).toBe(true);
    expect(ours[1]!.pcmHead, 'file block intact').toBe(fileSlot.pcmHead);
    expect(purged.result, 'the purge unloaded the unused project samples').toBeGreaterThanOrEqual(1);

    // B3: ReloadAllSamples(project settings, 1), queued on the worker by the firmware.
    const trace: { t: number; map: number[]; dsp: number[]; donor: number; key: number[] }[] = [];
    const invoker = await Promise.all([0, 1, 2, 3, 4, 5, 6, 7].map((i) => p.cf(0x4004eae4 + i * 4)));
    const reloaded = await mod.reload(1);
    const started = Date.now();
    try {
      await expect
        .poll(
          async () => {
            const map = [await p.cf(POOL.slotMap + 0x3ff * 4), await p.cf(POOL.slotMap + 0x3fe * 4)].map((v) => v | 0);
            const lengths = [(await p.slot(0x3ff)).dspLength, (await p.slot(0x3fe)).dspLength];
            const key = await p.words(POOL.slotKey + 0x3ff * 16, 4);
            trace.push({
              t: Date.now() - started,
              map,
              dsp: lengths,
              donor: (await p.slot(donor.slot)).dspLength,
              key,
            });
            return lengths.every((v) => v === 0) && map.every((v) => v === -1 || v === 0);
          },
          { timeout: 30_000, interval: 20 },
        )
        .toBe(true);
    } finally {
      record(out, 'reload', { reloaded, invoker: invoker.map((v) => v.toString(16)), trace });
    }
    const ourAfter = [await p.slot(0x3ff), await p.slot(0x3fe)];
    const stockAfter = await p.slot(donor.slot);
    record(out, 'after-reload', { ourAfter, stockAfter });
    expect(stockAfter.dspLength, 'the stock project slot is loaded again').toBeGreaterThan(0);
    await session.expectNoFault('stock pool borrow');
    writeFileSync(path.join(out, 'done'), 'ok');
  },
  { devices: ['dt2'], timeout: 600_000 },
);
