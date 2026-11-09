import { readFileSync } from 'node:fs';
import path from 'node:path';
import { Sample, prepareUpload } from '@digi/dk-session/dksample';
import { openSamples } from '@digi/dk-session/samples';
import { LOG_KIND } from '@digi/dk-session/gen_proto';
import { button, repoRoot } from '@digi/test-support';
import { expect } from 'vitest';
import type { PatchedSession } from '../support.ts';
import { liveArtifacts, record } from './artifacts.ts';
import { installWithDebug, moduleArtifact } from './install.ts';
import { runtimeTests } from './ready.ts';
import { planSymbol, prepareRuntimePlan } from './runtime-plan.ts';

/** Deterministic exact PCM16 fixture: mono48k avoids converter/resampler rounding. */
function proofWav(frames = 4096): { wav: Uint8Array; pcm: number[] } {
  const pcm = Array.from({ length: frames }, (_, n) => (((n * 71) % 101) - 50) * 256);
  const wav = Buffer.alloc(44 + pcm.length * 2);
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
  wav.writeUInt32LE(pcm.length * 2, 40);
  pcm.forEach((value, i) => wav.writeInt16LE(value, 44 + i * 2));
  return { wav, pcm };
}

/** Panel path and emulated timing from pk_harness.do_tap / Rig.select.
 * Runtime sessions run freely; elapsed is the input scheduler's clock. */
async function selectProofMachine({ session }: Pick<PatchedSession, 'session'>, artifacts: string): Promise<void> {
  const instructionsPerMs = 132_000;
  async function elapsed(): Promise<number> {
    const value = (await session.status()).elapsed;
    if (typeof value !== 'number' || !Number.isSafeInteger(value) || value < 0)
      throw new Error('machine selection requires the daemon status.elapsed instruction clock');
    return value;
  }
  async function until(target: number): Promise<void> {
    await expect.poll(elapsed, { timeout: 20_000, interval: 50 }).toBeGreaterThanOrEqual(target);
  }
  async function settle(ms: number): Promise<void> {
    await until((await elapsed()) + ms * instructionsPerMs);
  }
  async function tap(name: string): Promise<void> {
    const reply = (await session.client.inputTap(button(name), 20 * instructionsPerMs)) as { release_at?: unknown };
    if (typeof reply.release_at !== 'number' || !Number.isSafeInteger(reply.release_at))
      throw new Error('machine selection requires input.tap release_at on the elapsed clock');
    await until(reply.release_at + 5 * instructionsPerMs);
  }
  await tap('NO');
  await session.client.inputButton(button('FUNC'), true);
  try {
    await settle(5);
    await tap('SRC');
  } finally {
    await session.client.inputButton(button('FUNC'), false);
  }
  await settle(5);
  await settle(200);
  // Reset to the first row. The profile inserts the sole dynamic test machine
  // after the stock rows. DN2 has four audio machines PLUS MIDI: the
  // dynamic row is index5, confirmed by its live list (finding33).
  for (let i = 0; i < 16; i++) {
    await tap('UP');
    await settle(80);
  }
  const position = session.options.device === 'dt2' ? 7 : 5;
  for (let i = 0; i < position; i++) {
    await tap('DOWN');
    await settle(80);
  }
  record(artifacts, 'machine-list-selection', await session.client.framebufferGet({ hash: true, data: true }));
  await tap('YES');
  await settle(300);
  await tap('YES');
  await settle(300);
  record(artifacts, 'machine-page-after-selection', await session.client.framebufferGet({ hash: true, data: true }));
}

/** Capture a bounded second of coupled PCM while holding TRIG1. */
async function captureProofNote(
  runtime: Pick<PatchedSession, 'session'>,
  file: string,
): Promise<{ peak: number; samples: number }> {
  const { session } = runtime;
  await session.client.call('pcm.tap', { action: 'start' });
  await session.client.call('pcm.tap', { action: 'capture', path: file });
  try {
    await session.client.inputButton(button('TRIG 1'), true);
    await expect
      .poll(
        async () => {
          const report = (await session.client.call('pcm.tap', { action: 'report' })) as { samples: number };
          return report.samples;
        },
        { timeout: 60_000, interval: 250 },
      )
      .toBeGreaterThanOrEqual(96000);
  } finally {
    await session.client.inputButton(button('TRIG 1'), false);
    await session.client.call('pcm.tap', { action: 'capture_stop' });
  }
  // pcm.tap uses the emulator's fixed 44-byte IEEE-float WAV writer.
  const wav = readFileSync(file);
  expect(wav.length, 'capture contains a complete WAV header').toBeGreaterThanOrEqual(44);
  expect(wav.toString('ascii', 0, 4)).toBe('RIFF');
  expect(wav.readUInt32LE(4) + 8, 'RIFF size matches the completed capture').toBe(wav.length);
  expect(wav.toString('ascii', 8, 16)).toBe('WAVEfmt ');
  expect(wav.readUInt32LE(16)).toBe(16);
  expect(wav.readUInt16LE(20), 'IEEE float encoding').toBe(3);
  expect(wav.readUInt16LE(22)).toBe(2);
  expect(wav.readUInt32LE(24)).toBe(48000);
  expect(wav.readUInt32LE(28)).toBe(48000 * 8);
  expect(wav.readUInt16LE(32)).toBe(8);
  expect(wav.readUInt16LE(34)).toBe(32);
  expect(wav.toString('ascii', 36, 40)).toBe('data');
  const dataBytes = wav.readUInt32LE(40);
  expect(dataBytes, 'data chunk covers only the captured samples').toBe(wav.length - 44);
  expect(dataBytes % 8, 'complete interleaved stereo frames').toBe(0);
  const pcm = wav.subarray(44, 44 + dataBytes);
  expect(pcm.length, 'capture contains stereo half-second').toBeGreaterThanOrEqual(48000 * 4);
  let peak = 0;
  let finite = true;
  for (let i = 0; i < pcm.length; i += 4) {
    const value = pcm.readFloatLE(i);
    finite &&= Number.isFinite(value);
    peak = Math.max(peak, Math.abs(value));
  }
  expect(finite, 'captured PCM stays finite').toBe(true);
  return { peak, samples: pcm.length / 4 };
}

async function expectTransferredBank(runtime: PatchedSession): Promise<void> {
  const { session, dk } = runtime;
  const artifacts = liveArtifacts(session, 'sample-playback');
  const dispatchSymbol = session.options.device === 'dt2' ? '_dv_g' : '_dv_c';
  const dispatchOffset = session.options.device === 'dt2' ? 4 : 0;
  const plan = await prepareRuntimePlan(session, artifacts, ['dyndsp_info', dispatchSymbol, 'dl_link']);
  const artifact = moduleArtifact(session.options.device, 'test/sample-proof');
  const packed = readFileSync(artifact);
  const dsp = packed.subarray(packed.readUInt32BE(0x98), packed.readUInt32BE(0x98) + packed.readUInt32BE(0x9c));
  expect(dsp.readUInt32BE(4), 'fixture uses versioned sample-bank record').toBe(5);
  const bank = dsp.readUInt32BE(0x3c);
  expect(dsp.readUInt32LE(bank)).toBe(1);
  expect(dsp.readUInt32LE(bank + 4)).toBe(0);
  expect(dsp.readUInt32LE(bank + 12)).toBe(8192);
  const bankOffset = dsp.readUInt32LE(bank + 8);
  const viewOffset = dsp.readUInt32LE(bank + 16);
  await installWithDebug(runtime, 'test/sample-proof');
  async function bankBase(): Promise<number> {
    // MOD_DSP carries the selection index of the module, and digikit/debug is selected first.
    const loaded = (await dk.modules()).find((m) => m.name === 'test/sample-proof')!;
    expect(loaded).toBeDefined();
    expect(loaded.types).toHaveLength(1);
    await expect
      .poll(
        async () =>
          (await dk.readLog()).some(
            (e) => e.kind === LOG_KIND.MOD_DSP && e.args[0] === loaded.index && e.args[1] === 0,
          ),
        {
          timeout: 30_000,
          interval: 500,
        },
      )
      .toBe(true);
    const areaReply = (await session.client.call('mem.cf.peek', { addr: planSymbol(plan, 'dyndsp_info') + 12 })) as {
      value: number;
    };
    return areaReply.value + loaded.dsp_off;
  }
  let base = await bankBase();
  async function bytes(addr: number, len: number): Promise<Buffer> {
    const reply = (await session.client.call('dsp.peek', { addr, len })) as { hex: string; unknown: number };
    expect(reply.unknown, 'all published DSP bytes are readable').toBe(0);
    return Buffer.from(reply.hex, 'hex');
  }
  async function status(): Promise<number> {
    const reply = await dk.request(0x3f);
    expect(reply.status).toBe(0);
    expect(reply.body.length).toBe(4);
    return Buffer.from(reply.body).readInt32BE();
  }
  expect(await status(), 'bank begins UNUSED').toBe(0);
  expect(await bytes(base + viewOffset, 16), 'descriptor is unready before delivery').toEqual(Buffer.alloc(16));
  const sample = await Sample.create(
    Uint8Array.from(readFileSync(path.join(repoRoot(), 'apps/web/public/dksample.wasm'))),
  );
  const { wav, pcm } = proofWav();
  const prepared = prepareUpload(sample, 'test.wav', wav);
  expect(prepared.kind).toBe('converted');
  expect(prepared.stored.length).toBe(64 + pcm.length * 2 + 16);
  const backend = openSamples(session.options.device === 'dt2' ? 'stock-rpc' : 'digikit', {
    session: dk,
    hashOf: (data) => sample.hash(data),
  });
  await backend.mkdir('proof');
  const oversized = prepareUpload(sample, 'test.wav', proofWav(9000).wav);
  expect(oversized.kind).toBe('converted');
  expect(oversized.stored.length).toBe(64 + 9000 * 2 + 16);
  await backend.put('proof', oversized);
  expect((await backend.verify('proof', oversized)).ok, 'oversized source uploaded intact').toBe(true);
  expect((await dk.request(0x3e)).status, 'oversized acquisition queues before source validation').toBe(0);
  await expect.poll(status, { timeout: 60_000, interval: 250 }).toBe(-13);
  expect(await bytes(base + viewOffset, 16), 'oversized source never publishes any descriptor words').toEqual(
    Buffer.alloc(16),
  );
  expect(
    (await dk.modules())
      .filter((m) => m.name === 'test/sample-proof')
      .map((m) => ({ faults: m.faults, off: m.guard_off })),
  ).toEqual([{ faults: 0, off: false }]);
  await session.expectNoFault('oversized sample acquisition');
  record(artifacts, 'oversized', { status: -13, frames: 9000, base, viewOffset });

  // A failed bank is session-pinned. Restart the same selected module, proving
  // a new epoch before reading completion logs or recomputing its DSP address.
  const epoch = (await session.status()).epoch;
  expect(typeof epoch).toBe('number');
  await dk.restart();
  await dk.waitForBoot(); // requires fresh BOOT_START and no old RESTART event
  expect((await session.status()).epoch).toBeGreaterThan(epoch as number);
  base = await bankBase();
  expect(await status(), 'the new boot owns a clean UNUSED bank').toBe(0);
  expect(await bytes(base + viewOffset, 16), 'new descriptor begins unready').toEqual(Buffer.alloc(16));
  await selectProofMachine({ session }, artifacts);
  const unready = await captureProofNote({ session }, path.join(artifacts, 'unready.wav'));
  record(artifacts, 'unready-state', {
    status: await session.status(),
    faults: await session.faults(),
    unready,
  });
  await session.expectNoFault('unready proof-machine note');
  const expectedType = (await dk.modules()).find((m) => m.name === 'test/sample-proof')?.types[0];
  expect(expectedType, 'runtime assigned the proof machine a type').toBeDefined();
  // Adapter diagnostics differ: DT2 writes type to _dv_g byte4 (_dv_c word0
  // is its note-on flag); DN2 writes type to _dv_c word0. Resolve this exact
  // patch's symbol, then apply only the adapter's declared diagnostic offset.
  const dispatchAddress = planSymbol(plan, dispatchSymbol) + dispatchOffset;
  const dispatchedType = (await bytes(dispatchAddress, 4)).readUInt32LE();
  record(artifacts, 'unready-dispatch', { expectedType, dispatchedType, dispatchSymbol, dispatchAddress, unready });
  expect(dispatchedType, 'TRIG1 dispatched the runtime-assigned proof machine').toBe(expectedType);
  expect(unready.peak, 'notes before READY are silent').toBeLessThan(1e-6);

  // Replace the source only after failure completed and the clean boot is up.
  await backend.put('proof', prepared);
  expect((await backend.verify('proof', prepared)).ok, 'uploaded bytes verify before acquisition').toBe(true);
  const started = performance.now();
  expect((await dk.request(0x3e)).status, 'fixed path load queues successfully').toBe(0);
  let terminalStatus = 0;
  await expect
    .poll(
      async () => {
        terminalStatus = await status();
        return terminalStatus === 3 || terminalStatus < 0;
      },
      { timeout: 60_000, interval: 250 },
    )
    .toBe(true);
  const latencyMs = performance.now() - started;
  // Preserve terminal failure evidence before asserting READY. dl_link is the
  // stable 80-byte sender layout; raw peeks retain unknown bytes on errors.
  const linkAddress = planSymbol(plan, 'dl_link');
  const linkWords: (number | null)[] = [];
  for (let i = 0; i < 20; i++) linkWords.push(await session.client.memPeek(linkAddress + i * 4));
  record(artifacts, 'publication-state', {
    status: terminalStatus,
    sessionStatus: await session.status(),
    faults: await session.faults(),
    latencyMs,
    dlLink: { address: linkAddress, words: linkWords },
    descriptor: await session.client.call('dsp.peek', { addr: base + viewOffset, len: 16 }),
    bankHead: await session.client.call('dsp.peek', { addr: base + bankOffset, len: 64 }),
    log: await dk.readLog(),
    modules: await dk.modules(),
  });
  expect(terminalStatus, 'sample publication reaches READY without a terminal error').toBe(3);
  const view = await bytes(base + viewOffset, 16);
  expect([...Array(4)].map((_, i) => view.readUInt32LE(i * 4))).toEqual([
    1,
    0x08000000 + (base + bankOffset - 0x20000000) / 4,
    pcm.length,
    48000,
  ]);
  const wanted = Buffer.alloc(pcm.length * 4);
  pcm.forEach((value, i) => wanted.writeFloatLE(value / 32768, i * 4));
  expect(
    await bytes(base + bankOffset, wanted.length),
    'actual uploaded PCM reaches bank as exact float32 words',
  ).toEqual(wanted);
  expect((await dk.request(0x3e)).status, 'published bank refuses replacement').toBe(-10);
  expect(await status()).toBe(3);
  const ready = await captureProofNote({ session }, path.join(artifacts, 'ready.wav'));
  expect(ready.peak, 'the actual transferred sample is audible after READY').toBeGreaterThan(0.001);
  const retrigger = await captureProofNote({ session }, path.join(artifacts, 'retrigger.wav'));
  expect(retrigger.peak, 'a new note retriggers the immutable sample').toBeGreaterThan(0.001);
  record(artifacts, 'publication', {
    latencyMs,
    frames: pcm.length,
    base,
    bankOffset,
    viewOffset,
    unready,
    ready,
    retrigger,
  });
  expect(
    (await dk.modules())
      .filter((m) => m.name === 'test/sample-proof')
      .map((m) => ({ faults: m.faults, off: m.guard_off })),
  ).toEqual([{ faults: 0, off: false }]);
  await session.expectNoFault('transferred sample bank');
}

// Exact note-on/render PCM is checked by the fixture's own numerical tests.
// Coupled captures prove silence-before-READY and audible transferred playback/retrigger.
runtimeTests('transferred sample publishes an immutable mono bank', expectTransferredBank, {
  dsp: 'sharc',
  timeout: 900_000,
});
