import { execFile } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { promisify } from 'node:util';
import { button, repoRoot, resolveInputs, type Session } from '@digi/test-support';
import { describe, expect, test } from 'vitest';
import { withSession } from '../support.ts';
import { planSymbol, type RuntimePlan } from './runtime-plan.ts';
import { DdrWriter } from './sample-ddr-writer.ts';

const root = repoRoot();
const patchkit = process.env.DIGI_PATCHKIT_BIN;
// Explicit diagnostic opt-in. Generic runtime suites skip this file without preflighted binaries.
const devices = [
  { device: 'dn2', firmware: '1.12', binary: process.env.DIGI_EMUD_DN2 },
  { device: 'dt2', firmware: '1.17', binary: process.env.DIGI_EMUD_DT2 },
] as const;
const MAGIC = 0xddf4a601;
const TOKEN = 0x81234567;
const BASE = 0x80800000;
const BYTES = 960000;
const CANARY = 0xa55a3cc3;
const exec = promisify(execFile);

function checksum(words: number[]): number {
  let s1 = 0;
  let s2 = 0;
  for (const word of words) {
    s1 = (s1 + word) >>> 0;
    s2 = (s2 + s1) >>> 0;
  }
  return (s1 ^ s2) >>> 0;
}

function frame(seq: number, op: number, offset = 0, data: Buffer = Buffer.alloc(0), token = TOKEN): Buffer {
  if (data.length % 4 || data.length > 2560) throw new Error('payload must be at most 640 complete words');
  const header = [4, MAGIC, token, seq, op, offset, data.length / 4];
  const payload = Array.from({ length: data.length / 4 }, (_, i) => data.readUInt32LE(i * 4));
  const words = [...header, checksum([...header, ...payload]), ...Array<number>(24).fill(0), ...payload];
  const out = Buffer.alloc(2688);
  words.forEach((word, i) => {
    out.writeUInt16BE(word & 65535, i * 4);
    out.writeUInt16BE(word >>> 16, i * 4 + 2);
  });
  return out;
}

interface RawChunk {
  addr: number;
  len: number;
  hex: string;
  unknown: number;
}

async function rawPeek(session: Session, addr: number, length: number): Promise<RawChunk[]> {
  const chunks: RawChunk[] = [];
  for (let off = 0; off < length; off += 4096) {
    const len = Math.min(4096, length - off);
    const reply = (await session.client.call('dsp.peek', { addr: addr + off, len })) as {
      hex: string;
      unknown: number;
    };
    expect(reply.hex.length).toBe(len * 2);
    chunks.push({ addr: addr + off, len, hex: reply.hex, unknown: reply.unknown });
  }
  return chunks;
}

async function peek(session: Session, addr: number, length: number): Promise<Buffer> {
  const chunks = await rawPeek(session, addr, length);
  return Buffer.concat(
    chunks.map((chunk) => {
      expect(chunk.unknown, 'successfully written memory is fully known').toBe(0);
      expect(chunk.hex).toMatch(/^[0-9a-fA-F]+$/);
      return Buffer.from(chunk.hex, 'hex');
    }),
  );
}

async function prepare(session: Session) {
  const profile = devices.find(({ device }) => device === session.options.device);
  if (!profile) throw new Error(`unsupported DDR proof device: ${session.options.device}`);
  const inputs = resolveInputs(profile.device, profile.firmware);
  const output = path.join(session.runDir, 'ddr-proof.syx');
  const planFile = path.join(session.runDir, 'ddr-proof.plan.json');
  // Explicit prebuilt CLI: this diagnostic must never trigger a Cargo/daemon build.
  await exec(
    patchkit!,
    [
      '--root',
      root,
      'apply',
      '--firmware',
      inputs.syx,
      '--modules',
      'core/dsp-ddr-proof',
      '-o',
      output,
      '--plan',
      planFile,
    ],
    { cwd: root, timeout: 30000, maxBuffer: 4 * 1024 * 1024 },
  );
  const plan: RuntimePlan = JSON.parse(fs.readFileSync(planFile, 'utf8'));
  const stockSha = createHash('sha256').update(fs.readFileSync(inputs.syx)).digest('hex');
  expect(plan.output?.syx_sha256).toMatch(/^[a-f0-9]{64}$/);
  expect(plan.base.device).toBe(profile.device);
  expect(plan.base.version).toBe(profile.firmware);
  expect(plan.base.syx_sha256).toBe(stockSha);
  expect(plan.modules.map((module) => module.split('@')[0]).sort()).toEqual([
    'core/dsp-ddr-proof',
    'core/dsp-voice',
    'core/expansion',
    'core/machines',
    'core/params',
  ]);
  fs.writeFileSync(path.join(session.runDir, 'stock-identity.json'), JSON.stringify(session.loadResult, null, 2));
  let timer: ReturnType<typeof setTimeout> | undefined;
  let load;
  try {
    load = await Promise.race([
      session.client.firmwareLoad({ syx: output, base_sha256: stockSha, start: 'auto', auto_no: true }),
      new Promise<never>((_, reject) => {
        timer = setTimeout(() => reject(new Error('patched diagnostic startup exceeded 120000 ms')), 120000);
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
  expect(load.syx_sha256).toBe(plan.output?.syx_sha256);
  await session.client.call('run.mode', { mode: 'free' });
  await expect.poll(async () => (await session.status()).cf.ready, { timeout: 30000 }).toBe(true);
  const status = await session.status();
  expect(status.firmware?.base_sha256).toBe(stockSha);
  expect(status.firmware?.syx_sha256).toBe(plan.output?.syx_sha256);
  expect(status.firmware?.injected).toBe(true);
  fs.writeFileSync(path.join(session.runDir, 'patched-load.json'), JSON.stringify(load, null, 2));
  fs.writeFileSync(path.join(session.runDir, 'patched-status.json'), JSON.stringify(status, null, 2));
  const mail = planSymbol(plan, 'dd_mail');
  const staging = planSymbol(plan, 'dd_frame');
  const ready = planSymbol(plan, '_dd_ready');
  const words = planSymbol(plan, '_dd_words');
  async function request(data: Buffer, expectedState = 2, expectedError = 0): Promise<void> {
    const state = await session.client.memPeek(mail);
    expect([0, 2], 'one request in flight; error requires a fresh boot').toContain(state);
    await session.client.call('mem.cf.write', { addr: staging, hex: data.toString('hex') });
    await session.client.call('mem.cf.poke', { addr: mail, value: 4 }); // Publish last.
    await expect.poll(async () => session.client.memPeek(mail), { timeout: 30000, interval: 20 }).toBe(expectedState);
    const error = await session.client.memPeek(mail + 4);
    fs.appendFileSync(
      path.join(session.runDir, 'requests.jsonl'),
      JSON.stringify({
        header: data.subarray(0, 32).toString('hex'),
        state: expectedState,
        error,
        stall: await session.client.memPeek(mail + 8),
        sent: await session.client.memPeek(mail + 12),
      }) + '\n',
    );
    expect(error).toBe(expectedError);
  }
  async function dspWord(addr: number): Promise<number> {
    return (await peek(session, addr, 4)).readUInt32LE();
  }
  const writer = new DdrWriter(async (token, seq, op, offset, data) => {
    await request(frame(seq, op, offset, data, token), 2, op === 3 ? 1 : op === 4 ? 5 : op === 5 ? 7 : 0);
    if (op === 2 && seq === 1) {
      const before = await peek(session, BASE, 2560);
      await request(frame(seq, op, offset, data, token));
      expect(await dspWord(words)).toBe(640);
      expect(await peek(session, BASE, 2560)).toEqual(before);
    }
    if (op === 3) {
      expect(await dspWord(ready), 'COMMIT seals without publishing READY').toBe(0);
      expect(await dspWord(planSymbol(plan, '_dd_admission_open'))).toBe(0);
    }
  });
  return { request, writer, ready: () => dspWord(ready), words: () => dspWord(words), plan };
}

function quietProof(name: string, body: (session: Session) => Promise<void>) {
  for (const { device, firmware, binary } of devices) {
    const preflighted = Boolean(binary && patchkit);
    const deviceName = `${device} ${firmware}: ${name}`;
    describe.skipIf(!preflighted)(deviceName, () => {
      const runDir = path.join(root, 'out/runs/ddr-proof', device, randomUUID());
      if (preflighted) fs.mkdirSync(runDir, { recursive: true });
      const handle = withSession({
        runDir,
        device,
        firmware,
        dsp: 'sharc',
        binary,
        restart: false,
      });
      test(
        deviceName,
        async () => {
          await expect.poll(async () => (await handle.session.status()).cf.ready, { timeout: 30000 }).toBe(true);
          fs.writeFileSync(path.join(runDir, 'identity.json'), JSON.stringify(handle.session.loadResult, null, 2));
          try {
            await body(handle.session);
            await handle.session.expectNoFault(deviceName);
            fs.writeFileSync(
              path.join(runDir, 'result.json'),
              JSON.stringify({ name: deviceName, device, firmware, passed: true }),
            );
          } catch (error) {
            fs.writeFileSync(
              path.join(runDir, 'result.json'),
              JSON.stringify({ name: deviceName, device, firmware, passed: false, error: String(error) }),
            );
            throw error;
          }
        },
        360000,
      );
    });
  }
}

interface Registry {
  name: string;
  addr: number;
  first: number;
  row_size: number;
  fields: [string, string, number][];
  entries: { id: string; index: number; module: string }[];
}

async function playbackProof(
  session: Session,
  plan: RuntimePlan,
  retire?: () => Promise<void>,
  replace?: () => Promise<{ token: number; data: Buffer }>,
): Promise<void> {
  const registries = (plan as RuntimePlan & { registries: Registry[] }).registries;
  const machines = registries.find((registry) => registry.name === 'machines')!;
  const voices = registries.find((registry) => registry.name === 'voices')!;
  const machine = machines.entries.find((entry) => entry.id === 'ddr_proof')!;
  const voice = voices.entries.find((entry) => entry.id === 'ddr_proof')!;
  expect(machine.module.split('@')[0]).toBe('core/dsp-ddr-proof');
  const field = (registry: Registry, index: number, name: string) => {
    const offset = registry.fields.find(([key]) => key === name)?.[2];
    if (offset === undefined) throw new Error(`missing ${registry.name}.${name}`);
    return registry.addr + (index - registry.first) * registry.row_size + offset;
  };
  const insertionPosition = await session.client.memPeek(field(machines, machine.index, 'position'));
  expect(typeof insertionPosition).toBe('number');
  expect(insertionPosition).toBeGreaterThanOrEqual(0);
  expect(insertionPosition, 'diagnostic is listed rather than a gap row').not.toBe(0xffffffff);
  const word = async (addr: number) => (await peek(session, addr, 4)).readUInt32LE();
  expect(await word(field(voices, voice.index, 'type'))).toBe(machine.index);
  expect(await word(field(voices, voice.index, 'pc'))).toBe(planSymbol(plan, '_dd_play_render'));
  expect(await word(field(voices, voice.index, 'note_on'))).toBe(planSymbol(plan, '_dd_play_note_on'));
  const descriptor = await peek(session, planSymbol(plan, '_dd_play_desc'), 40);
  const descriptorWords = Array.from({ length: 10 }, (_, i) => descriptor.readUInt32LE(i * 4));
  expect(descriptorWords.slice(2, 6)).toEqual([1, 16, 28, 28]);
  fs.writeFileSync(
    path.join(session.runDir, 'playback-binding.json'),
    JSON.stringify(
      {
        machine,
        insertionPosition,
        voice,
        descriptorWords,
        descriptorHex: descriptor.toString('hex'),
        patchedImageHash: plan.output?.syx_sha256,
      },
      null,
      2,
    ),
  );
  const normalAddress = planSymbol(plan, '_dd_normal_frames');
  const normalBefore = await word(normalAddress);
  await expect.poll(async () => word(normalAddress), { timeout: 10000, interval: 25 }).toBeGreaterThan(normalBefore);

  // Existing panel path on the emulated instruction clock. The displayed list
  // position comes from the live patched registry, rather than a fixed type.
  const clock = async () => {
    const value = (await session.status()).elapsed;
    if (typeof value !== 'number') throw new Error('panel proof requires elapsed instruction clock');
    return value;
  };
  const until = async (target: number) => {
    await expect.poll(clock, { timeout: 20000, interval: 25 }).toBeGreaterThanOrEqual(target);
  };
  const settle = async (ms: number) => until((await clock()) + ms * 132000);
  const tap = async (name: string) => {
    const reply = (await session.client.inputTap(button(name), 20 * 132000)) as { release_at: number };
    expect(typeof reply.release_at).toBe('number');
    await until(reply.release_at + 5 * 132000);
  };
  await tap('NO');
  if (session.options.device === 'dt2') {
    await session.client.inputButton(button('TRK'), true);
    try {
      await settle(5);
      await tap('TRIG 1');
    } finally {
      await session.client.inputButton(button('TRK'), false);
    }
    await settle(100);
  }
  await session.client.inputButton(button('FUNC'), true);
  try {
    await settle(5);
    await tap('SRC');
  } finally {
    await session.client.inputButton(button('FUNC'), false);
  }
  await settle(200);
  // Both machine adapters build this exported buffer on menu entry. The shared
  // manifest's list.alloc/list.operands use first + entries, excluding gap rows.
  const listLength = machines.first + machines.entries.length;
  expect(listLength).toBeGreaterThan(0);
  expect(listLength).toBeLessThanOrEqual(16);
  const listAddress = planSymbol(plan, 'mf_list_buf');
  const menuTypes: number[] = [];
  for (let i = 0; i < listLength; i++) {
    const type = await session.client.memPeek(listAddress + i * 4);
    expect(typeof type, 'live menu type is mapped').toBe('number');
    menuTypes.push(type!);
  }
  const expectedTypes = [
    ...Array.from({ length: machines.first }, (_, type) => type),
    ...machines.entries.map((entry) => entry.index),
  ];
  const position = menuTypes.indexOf(machine.index);
  fs.writeFileSync(
    path.join(session.runDir, 'machine-menu.json'),
    JSON.stringify({ insertionPosition, listAddress, listLength, menuTypes, expectedTypes, position }, null, 2),
  );
  expect([...menuTypes].sort((a, b) => a - b)).toEqual(expectedTypes.sort((a, b) => a - b));
  expect(
    menuTypes.filter((type) => type === machine.index),
    'diagnostic occurs once in the live menu',
  ).toHaveLength(1);
  expect(position).toBeGreaterThanOrEqual(0);
  expect(position).toBeLessThan(listLength);
  for (let i = 0; i < 16; i++) {
    await tap('UP');
    await settle(40);
  }
  for (let i = 0; i < position; i++) {
    await tap('DOWN');
    await settle(80);
  }
  fs.writeFileSync(
    path.join(session.runDir, 'machine-list.json'),
    JSON.stringify(await session.client.framebufferGet({ hash: true, data: true })),
  );
  await tap('YES');
  await settle(300);
  await tap('YES');
  await settle(300);
  fs.writeFileSync(
    path.join(session.runDir, 'machine-page.json'),
    JSON.stringify(await session.client.framebufferGet({ hash: true, data: true })),
  );

  const stateAddress = planSymbol(plan, '_dd_play_state');
  const notesAddress = planSymbol(plan, '_dd_play_notes');
  const blocksAddress = planSymbol(plan, '_dd_play_blocks');
  const doneAddress = planSymbol(plan, '_dd_play_done');
  const normalAtNote = await word(normalAddress);
  const notesBefore = await word(notesAddress);
  const snapshots: {
    cursor: number[];
    notes: number;
    blocks: number;
    done: number;
    normal: number;
    seconds: number;
  }[] = [];
  const wavPath = path.join(session.runDir, 'ten-second-ddr.wav');
  await session.client.call('pcm.tap', { action: 'start' });
  await session.client.call('pcm.tap', { action: 'capture', path: wavPath });
  try {
    await session.client.inputButton(button('TRIG 1'), true);
    await expect
      .poll(
        async () => {
          const state = await word(stateAddress);
          const notes = await word(notesAddress);
          if (!state || notes === notesBefore) return false;
          const cursorBytes = await peek(session, state, 24);
          const cursor = Array.from({ length: 6 }, (_, i) => cursorBytes.readUInt32LE(i * 4));
          const tapReport = (await session.client.call('pcm.tap', { action: 'report' })) as { seconds: number };
          const sample = {
            cursor,
            notes,
            blocks: await word(blocksAddress),
            done: await word(doneAddress),
            normal: await word(normalAddress),
            seconds: tapReport.seconds,
          };
          snapshots.push(sample);
          return retire
            ? cursor[2]! >= 48000 && cursor[2]! < 432000 && sample.done === 0
            : sample.done === 1 && tapReport.seconds >= 11.5;
        },
        { timeout: 180000, interval: 50 },
      )
      .toBe(true);
    if (retire) {
      const atRequest = snapshots.at(-1)!;
      await retire(); // Exact token/seq/op, CF DONE only for DSP RETIRED status 5.
      const control = async () => ({
        ready: await word(planSymbol(plan, '_dd_ready')),
        open: await word(planSymbol(plan, '_dd_admission_open')),
        closed: await word(planSymbol(plan, '_dd_retire_closed')),
        admitted: await word(planSymbol(plan, '_dd_admitted')),
        fault: await word(planSymbol(plan, '_dd_admit_fault')),
        reads: await word(planSymbol(plan, '_dd_play_reads')),
      });
      const retired = await control();
      expect(retired).toMatchObject({ ready: 0, open: 0, closed: 1, admitted: 0, fault: 0 });
      expect(retired.reads).toBeGreaterThan(0);
      expect(retired.reads).toBeLessThan(15000);
      const oldBank = await peek(session, BASE, BYTES);
      const guardsBefore = [await peek(session, BASE - 4096, 4096), await peek(session, BASE + BYTES, 4096)];
      const canary = Buffer.alloc(4096);
      for (let i = 0; i < 1024; i++) canary.writeUInt32LE(CANARY, 4 * i);
      expect(guardsBefore).toEqual([canary, canary]);
      const poisoned = Buffer.alloc(BYTES, 0x5a);
      const patchFile = path.join(session.runDir, 'retired-bank.patch');
      fs.writeFileSync(
        patchFile,
        `BASE ${plan.base.syx_sha256}\nBYTES ${BASE.toString(16)} ${poisoned.toString('hex')}\n`,
      );
      // Existing diagnostic memory edit API; no transport or firmware overwrite
      // occurs before the exact retirement acknowledgment above.
      const injected = await session.client.call('dsp.patch', { file: patchFile });
      const afterBank = await peek(session, BASE, BYTES);
      expect(afterBank.equals(poisoned)).toBe(true);
      const normalAfterPoison = await word(normalAddress);
      const observations = [retired];
      const observationFile = path.join(session.runDir, 'retirement-observations.json');
      fs.writeFileSync(observationFile, JSON.stringify(observations, null, 2));
      const unchanged = async () => {
        const sample = await control();
        observations.push(sample);
        fs.writeFileSync(observationFile, JSON.stringify(observations, null, 2));
        expect(sample).toEqual(retired);
      };
      await settle(1200); // Keep the original note held across backing reuse.
      await unchanged();
      await expect
        .poll(async () => word(normalAddress), { timeout: 10000, interval: 25 })
        .toBeGreaterThan(normalAfterPoison);
      const heldState = await word(stateAddress);
      expect((await peek(session, heldState, 24)).readUInt32LE(20)).toBe(1);
      await session.expectNoFault('retired bank with original note held');
      const notesBeforeRetrigger = await word(planSymbol(plan, '_dd_obs_note_entries'));
      await session.client.inputButton(button('TRIG 1'), false);
      await settle(300);
      await tap('TRIG 1'); // A fresh note must also remain terminally denied.
      await settle(1200);
      await expect
        .poll(async () => word(planSymbol(plan, '_dd_obs_note_entries')), { timeout: 10000, interval: 25 })
        .toBeGreaterThan(notesBeforeRetrigger);
      await unchanged();
      await retire(); // Duplicate terminal request remains idempotent.
      await unchanged();
      const finalState = await word(stateAddress);
      expect((await peek(session, finalState, 24)).readUInt32LE(20)).toBe(1);
      const guardsAfter = [await peek(session, BASE - 4096, 4096), await peek(session, BASE + BYTES, 4096)];
      expect(guardsAfter).toEqual(guardsBefore);
      const context: Record<string, unknown> = {};
      for (const kind of ['note', 'render', 'tail']) {
        context[kind] = {
          entries: await word(planSymbol(plan, `_dd_obs_${kind}_entries`)),
          exits: await word(planSymbol(plan, `_dd_obs_${kind}_exits`)),
          first: (await peek(session, planSymbol(plan, `_dd_obs_${kind}_first`), 24)).toString('hex'),
          last: (await peek(session, planSymbol(plan, `_dd_obs_${kind}_last`), 24)).toString('hex'),
        };
      }
      fs.writeFileSync(
        path.join(session.runDir, 'retirement-result.json'),
        JSON.stringify(
          {
            atRequest,
            normalAfterPoison,
            notesBeforeRetrigger,
            retired,
            observations,
            injected,
            context,
            tailSawActive: await word(planSymbol(plan, '_dd_obs_tail_active')),
            beforeSha256: createHash('sha256').update(oldBank).digest('hex'),
            afterSha256: createHash('sha256').update(afterBank).digest('hex'),
            guardSha256: guardsAfter.map((guard) => createHash('sha256').update(guard).digest('hex')),
            wavPath,
            scope: 'Terminal bank only; no reopen or upstream event-age policy. Stock effect tails may remain audible.',
          },
          null,
          2,
        ),
      );
      if (replace) {
        // Keep a trigger held through closed-bank transport. Only exact DSP ACKs
        // grant CF substitution; no quiet-note assumption permits backing reuse.
        await session.client.inputButton(button('TRIG 1'), true);
        await settle(100);
        const replacement = await replace();
        expect(await peek(session, BASE, BYTES)).toEqual(replacement.data);
        expect(await peek(session, BASE - 4096, 4096)).toEqual(canary);
        expect(await peek(session, BASE + BYTES, 4096)).toEqual(canary);
        const readsBeforeNewNote = await word(planSymbol(plan, '_dd_play_reads'));
        const noteEntries = await word(planSymbol(plan, '_dd_obs_note_entries'));
        // A fresh/late note after READY resolves the selected current bank.
        await session.client.inputButton(button('TRIG 1'), false);
        await settle(100);
        await session.client.call('pcm.tap', { action: 'capture_stop' });
        const replacementWav = path.join(session.runDir, 'generation-c.wav');
        await session.client.call('pcm.tap', { action: 'start' });
        await session.client.call('pcm.tap', { action: 'capture', path: replacementWav });
        await session.client.inputButton(button('TRIG 1'), true);
        await expect
          .poll(async () => word(planSymbol(plan, '_dd_obs_note_entries')), { timeout: 10000 })
          .toBeGreaterThan(noteEntries);
        await settle(1500);
        await session.client.call('pcm.tap', { action: 'capture_stop' });
        const currentState = await word(stateAddress);
        expect(await word(currentState + 24)).toBe(replacement.token);
        expect(await word(planSymbol(plan, '_dd_play_reads'))).toBeGreaterThan(readsBeforeNewNote);
        const pcm = fs.readFileSync(replacementWav).subarray(44);
        let finite = true;
        for (let i = 0; i < pcm.length; i += 4) finite &&= Number.isFinite(pcm.readFloatLE(i));
        const samples = Array.from({ length: 24000 }, (_, i) => {
          const offset = (14400 + i) * 8;
          return (pcm.readFloatLE(offset) + pcm.readFloatLE(offset + 4)) * 0.5;
        });
        const power = (hz: number) => {
          let real = 0;
          let imag = 0;
          samples.forEach((sample, i) => {
            real += sample * Math.cos((2 * Math.PI * hz * i) / 48000);
            imag += sample * Math.sin((2 * Math.PI * hz * i) / 48000);
          });
          return real * real + imag * imag;
        };
        const powers = [200, 300, 400].map(power);
        const rms = Math.sqrt(samples.reduce((sum, sample) => sum + sample * sample, 0) / samples.length);
        const audio = {
          finite,
          capturedFrames: pcm.length / 8,
          windowStartFrame: 14400,
          windowEndFrame: 38400,
          rms,
          frequencies: [200, 300, 400],
          powers,
        };
        // Retain measurements before the audibility/content verdicts.
        fs.writeFileSync(path.join(session.runDir, 'generation-c-audio.json'), JSON.stringify(audio, null, 2));
        fs.writeFileSync(
          path.join(session.runDir, 'generation-reopening-result.json'),
          JSON.stringify(
            {
              token: replacement.token,
              sourceSha256: createHash('sha256').update(replacement.data).digest('hex'),
              fullBankSha256: createHash('sha256')
                .update(await peek(session, BASE, BYTES))
                .digest('hex'),
              guardSha256: createHash('sha256').update(canary).digest('hex'),
              noteEntriesBefore: noteEntries,
              readsBeforeNewNote,
              cursor: (await peek(session, currentState, 28)).toString('hex'),
              frequencies: [200, 300, 400],
              powers,
              audio,
              replacementWav,
              scope:
                'A retired while active; B sealed and retired without opening; C explicitly opened and a fresh note uses C.',
            },
            null,
            2,
          ),
        );
        expect(finite, 'entire generation C capture is finite').toBe(true);
        expect(rms, 'generation C is audible in the measured window').toBeGreaterThan(0.001);
        expect(powers[2]!).toBeGreaterThan(100 * Math.max(powers[0]!, powers[1]!));
      }
    }
  } finally {
    await session.client.inputButton(button('TRIG 1'), false);
    await session.client.call('pcm.tap', { action: 'capture_stop' });
    fs.writeFileSync(path.join(session.runDir, 'playback-states.json'), JSON.stringify(snapshots, null, 2));
  }
  if (retire) return;
  const final = snapshots.at(-1)!;
  // Adapter diagnostics differ: DT2 guard word 1 is type; DN2 contract word 0 is type.
  const dispatchedTypeAddress =
    session.options.device === 'dt2' ? planSymbol(plan, '_dv_g') + 4 : planSymbol(plan, '_dv_c');
  const dispatchedType = await word(dispatchedTypeAddress);
  const output = await word(planSymbol(plan, '_dd_play_output'));
  const completedOutput = await peek(session, output, 128);
  // This is the stock engine's workspace, which may be reused downstream.
  // The numerical consumer test proves the extra render after DONE is zero.

  const wav = fs.readFileSync(wavPath);
  expect(wav.toString('ascii', 0, 4)).toBe('RIFF');
  expect(wav.readUInt16LE(20)).toBe(3);
  expect(wav.readUInt16LE(22)).toBe(2);
  expect(wav.readUInt32LE(24)).toBe(48000);
  expect(wav.readUInt32LE(40) + 44).toBe(wav.length);
  const pcm = wav.subarray(44);
  expect(pcm.length / 8).toBeGreaterThanOrEqual(11.5 * 48000);
  let finite = true;
  for (let i = 0; i < pcm.length; i += 4) finite &&= Number.isFinite(pcm.readFloatLE(i));
  expect(finite).toBe(true);
  const window = (start: number) => {
    const samples = Array.from({ length: 48000 }, (_, i) => {
      const offset = (start * 48000 + i) * 8;
      return (pcm.readFloatLE(offset) + pcm.readFloatLE(offset + 4)) * 0.5;
    });
    const rms = Math.sqrt(samples.reduce((sum, value) => sum + value * value, 0) / samples.length);
    const correlation = (lag: number) => {
      let dot = 0;
      let a = 0;
      let b = 0;
      for (let i = 0; i < samples.length - lag; i++) {
        dot += samples[i]! * samples[i + lag]!;
        a += samples[i]! ** 2;
        b += samples[i + lag]! ** 2;
      }
      return dot / Math.sqrt(a * b);
    };
    return { rms, period: correlation(240), halfPeriod: correlation(120) };
  };
  const early = window(1);
  const late = window(8);

  // Sequential live reads can straddle callbacks. These bounded snapshots and
  // non-atomic markers describe observations only; zero overlap cannot prove
  // exclusion, and ASTATX (including its low 12 bits) is not a task identity.
  const context = async (name: string) => {
    const bytes = await peek(session, planSymbol(plan, name), 24);
    const words = Array.from({ length: 6 }, (_, i) => bytes.readUInt32LE(i * 4));
    return {
      mode1: words[0],
      astatx: words[1],
      astatxLow12: words[1]! & 0xfff,
      i6: words[2],
      i7: words[3],
      i13: words[4],
      active: words[5],
    };
  };
  const scheduling: Record<string, unknown> = {
    active: await word(planSymbol(plan, '_dd_obs_active')),
    tailSawActive: await word(planSymbol(plan, '_dd_obs_tail_active')),
  };
  for (const kind of ['note', 'render', 'tail']) {
    scheduling[kind] = {
      entries: await word(planSymbol(plan, `_dd_obs_${kind}_entries`)),
      exits: await word(planSymbol(plan, `_dd_obs_${kind}_exits`)),
      first: await context(`_dd_obs_${kind}_first`),
      last: await context(`_dd_obs_${kind}_last`),
    };
  }
  fs.writeFileSync(
    path.join(session.runDir, 'playback-result.json'),
    JSON.stringify(
      {
        machine,
        position,
        voice,
        descriptorWords,
        descriptorHex: descriptor.toString('hex'),
        patchedImageHash: plan.output?.syx_sha256,
        final,
        normalBefore,
        normalAtNote,
        dispatchedType,
        finite,
        capturedFrames: pcm.length / 8,
        scheduling,
        early,
        late,
        output,
        completedOutput: completedOutput.toString('hex'),
        wavPath,
      },
      null,
      2,
    ),
  );
  expect(final.notes).toBe(notesBefore + 1);
  expect(final.cursor).toEqual([0, 480000, 480000, 0, 65536, 1]);
  expect(final.blocks).toBe(15000);
  expect(snapshots.some((sample) => sample.cursor[2]! > 0 && sample.cursor[2]! < 240000)).toBe(true);
  expect(
    snapshots.some((sample) => sample.cursor[2]! >= 432000 && sample.cursor[2]! < 480000 && sample.done === 0),
  ).toBe(true);
  expect(final.normal).toBeGreaterThan(normalAtNote + 15000);
  expect(dispatchedType).toBe(machine.index);
  for (const content of [early, late]) {
    expect(content.rms, 'audible sustained source at early and late windows').toBeGreaterThan(0.001);
    expect(content.period, 'known 200 Hz source period survives playback').toBeGreaterThan(0.7);
    expect(content.halfPeriod).toBeLessThan(-0.2);
  }
}

quietProof('full quiet DDR transport: 960000 bytes, complete canaries and duplicate', async (session) => {
  const proof = await prepare(session);
  // Capture the entire candidate range and guard bands before INIT owns them.
  const baseline = await rawPeek(session, BASE - 4096, BYTES + 8192);
  fs.writeFileSync(path.join(session.runDir, 'ddr-before-init.json'), JSON.stringify(baseline));
  const data = Buffer.alloc(BYTES);
  for (let i = 0; i < BYTES / 2; i++) {
    const triangle = 8192 * (1 - 4 * Math.abs((i % 240) / 240 - 0.5));
    const fade = Math.min(1, i / 960, (479999 - i) / 960);
    data.writeInt16LE(Math.round(triangle * fade), i * 2);
  }
  await proof.writer.load(data, TOKEN);
  expect(await proof.words()).toBe(240000);
  expect(await proof.ready()).toBe(1);
  const after = await peek(session, BASE, BYTES);
  expect(after.equals(data), 'entire transferred payload, not a prefix or hash only').toBe(true);
  const canary = Buffer.alloc(4096);
  for (let i = 0; i < 1024; i++) canary.writeUInt32LE(CANARY, 4 * i);
  expect(await peek(session, BASE - 4096, 4096)).toEqual(canary);
  expect(await peek(session, BASE + BYTES, 4096)).toEqual(canary);
  fs.writeFileSync(
    path.join(session.runDir, 'ddr-proof-result.json'),
    JSON.stringify(
      {
        bytes: BYTES,
        sha256: createHash('sha256').update(after).digest('hex'),
        baselineSha256: createHash('sha256').update(JSON.stringify(baseline)).digest('hex'),
        canaryBytes: 8192,
        ready: await proof.ready(),
        plan: proof.plan,
      },
      null,
      2,
    ),
  );
  await playbackProof(session, proof.plan);
});

quietProof('DDR out-of-range DATA poisons proof without READY', async (session) => {
  const proof = await prepare(session);
  await proof.request(frame(0, 1));
  const before = await rawPeek(session, BASE - 4096, BYTES + 8192);
  fs.writeFileSync(path.join(session.runDir, 'ddr-before-rejection.json'), JSON.stringify(before));
  await proof.request(frame(1, 2, 240000, Buffer.from([1, 2, 3, 4])), 3, 101);
  expect(await proof.ready()).toBe(0);
  expect(await proof.words()).toBe(0);
  const after = await rawPeek(session, BASE - 4096, BYTES + 8192);
  fs.writeFileSync(path.join(session.runDir, 'ddr-after-rejection.json'), JSON.stringify(after));
  expect(after).toEqual(before);
});

quietProof('DDR partial checksum failure retains backing and never publishes READY', async (session) => {
  const proof = await prepare(session);
  await proof.request(frame(0, 1));
  await proof.request(frame(1, 2, 0, Buffer.alloc(2560, 0x59)));
  const before = await rawPeek(session, BASE - 4096, BYTES + 8192);
  fs.writeFileSync(path.join(session.runDir, 'ddr-before-rejection.json'), JSON.stringify(before));
  const bad = frame(2, 2, 640, Buffer.alloc(2560, 0x96));
  bad[28] = bad[28]! ^ 1; // Corrupt checksum, leaving token/sequence/range valid.
  await proof.request(bad, 3, 101);
  expect(await proof.ready()).toBe(0);
  expect(await proof.words()).toBe(640);
  const after = await rawPeek(session, BASE - 4096, BYTES + 8192);
  fs.writeFileSync(path.join(session.runDir, 'ddr-after-rejection.json'), JSON.stringify(after));
  expect(after).toEqual(before);
});

quietProof('DDR stale token cannot adopt or write; CF watchdog resumes stock frames', async (session) => {
  const proof = await prepare(session);
  await proof.request(frame(0, 1));
  const before = await rawPeek(session, BASE - 4096, BYTES + 8192);
  fs.writeFileSync(path.join(session.runDir, 'ddr-before-rejection.json'), JSON.stringify(before));
  await proof.request(frame(1, 2, 0, Buffer.alloc(2560, 0x66), TOKEN + 1), 3, 100);
  expect(await proof.ready()).toBe(0);
  expect(await proof.words()).toBe(0);
  const after = await rawPeek(session, BASE - 4096, BYTES + 8192);
  fs.writeFileSync(path.join(session.runDir, 'ddr-after-rejection.json'), JSON.stringify(after));
  expect(after).toEqual(before);
});

quietProof(
  'terminal DDR retirement drains before poison and denies held and retriggered consumers',
  async (session) => {
    const proof = await prepare(session);
    const data = Buffer.alloc(BYTES);
    for (let i = 0; i < BYTES / 2; i++) {
      const triangle = 8192 * (1 - 4 * Math.abs((i % 240) / 240 - 0.5));
      const fade = Math.min(1, i / 960, (479999 - i) / 960);
      data.writeInt16LE(Math.round(triangle * fade), i * 2);
    }
    await proof.writer.load(data, TOKEN);
    expect(await proof.ready()).toBe(1);
    expect(await peek(session, BASE, BYTES)).toEqual(data);
    await playbackProof(session, proof.plan, () => proof.request(frame(377, 4), 2, 5));
  },
);

function triangleBank(hz: number): Buffer {
  const data = Buffer.alloc(BYTES);
  const period = 48000 / hz;
  for (let i = 0; i < BYTES / 2; i++) {
    const triangle = 8192 * (1 - 4 * Math.abs((i % period) / period - 0.5));
    const fade = Math.min(1, i / 960, (479999 - i) / 960);
    data.writeInt16LE(Math.round(triangle * fade), i * 2);
  }
  return data;
}

quietProof('DDR generation A retires, sealed B is skipped, and C opens for a new note', async (session) => {
  const proof = await prepare(session);
  const a = triangleBank(200);
  await proof.writer.load(a, TOKEN);
  expect(await peek(session, BASE, BYTES)).toEqual(a);
  await playbackProof(
    session,
    proof.plan,
    () => proof.request(frame(377, 4), 2, 5),
    async () => {
      const b = triangleBank(300);
      const c = triangleBank(400);
      // C must retire sealed B before its next INIT.
      const loadingB = proof.writer.load(b, TOKEN + 1, false);
      const loadingC = loadingB.then(async () => {
        expect(await proof.ready()).toBe(0);
        expect(await peek(session, BASE, BYTES)).toEqual(b);
        const canary = Buffer.alloc(4096);
        for (let i = 0; i < 1024; i++) canary.writeUInt32LE(CANARY, i * 4);
        const guards = [await peek(session, BASE - 4096, 4096), await peek(session, BASE + BYTES, 4096)];
        expect(guards).toEqual([canary, canary]);
        fs.writeFileSync(
          path.join(session.runDir, 'generation-b-sealed.json'),
          JSON.stringify({
            token: TOKEN + 1,
            ready: await proof.ready(),
            guardSha256: guards.map((guard) => createHash('sha256').update(guard).digest('hex')),
            sha256: createHash('sha256')
              .update(await peek(session, BASE, BYTES))
              .digest('hex'),
          }),
        );
        return proof.writer.load(c, TOKEN + 2);
      });
      await loadingC;
      expect(await proof.ready()).toBe(1);
      return { token: TOKEN + 2, data: c };
    },
  );
  const word = async (name: string) => (await peek(session, planSymbol(proof.plan, name), 4)).readUInt32LE();
  const before = await peek(session, BASE - 4096, BYTES + 8192);
  const current = {
    token: await word('_dd_token'),
    ready: await proof.ready(),
    open: await word('_dd_admission_open'),
  };
  expect(current).toEqual({ token: TOKEN + 2, ready: 1, open: 1 });
  // A valid stale B RETIRE bypasses quiet masks and actually reaches DSP.
  // C's ACK identity cannot complete it; timeout permanently stops the CF writer.
  await proof.request(frame(377, 4, 0, Buffer.alloc(0), TOKEN + 1), 3, 100);
  const mail = planSymbol(proof.plan, 'dd_mail');
  expect(await session.client.memPeek(mail + 12)).toBeGreaterThan(0);
  expect(await session.client.memPeek(mail)).toBe(3);
  expect({
    token: await word('_dd_token'),
    ready: await proof.ready(),
    open: await word('_dd_admission_open'),
  }).toEqual(current);
  expect(await peek(session, BASE - 4096, BYTES + 8192)).toEqual(before);
  const normalBefore = await word('_dd_normal_frames');
  await expect.poll(() => word('_dd_normal_frames'), { timeout: 10000 }).toBeGreaterThan(normalBefore);
  expect(await session.client.memPeek(mail)).toBe(3);
  fs.writeFileSync(
    path.join(session.runDir, 'stale-b-retire-result.json'),
    JSON.stringify(
      {
        current,
        error: await session.client.memPeek(mail + 4),
        sent: await session.client.memPeek(mail + 12),
        fullBankAndGuardsSha256: createHash('sha256').update(before).digest('hex'),
        normalBefore,
        normalAfter: await word('_dd_normal_frames'),
      },
      null,
      2,
    ),
  );
});
