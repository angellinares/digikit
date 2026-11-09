import { execFile } from 'node:child_process';
import { createHash, randomUUID } from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { promisify } from 'node:util';
import { button, repoRoot, resolveInputs } from '@digi/test-support';
import { describe, expect, test } from 'vitest';
import { withSession } from '../support.ts';
import { planSymbol, type RuntimePlan } from './runtime-plan.ts';

const root = repoRoot();
const binary = process.env.DIGI_LP0_EMUD;
const patchkit = process.env.DIGI_PATCHKIT_BIN;
const enabled = Boolean(binary && patchkit);
const exec = promisify(execFile);
const BASE = 0x80800000;
const BANK_BYTES = 960000;
const PAGE_BYTES = 4096;
const CANARY = 0xa55a3cc3;
// Canonical host byte alias of the driver's internal 0x268ca8 record.
const PORT = 0x28268ca8;
const DMA = 0x30fff000;
const LP = 0x30ffe000;
const globals = ['_lp_status', '_lp_error', '_lp_pages', '_lp_bytes', '_lp_state', '_lp_rx', '_lp_handle'] as const;

interface RawMemory {
  addr: number;
  len: number;
  hex: string;
  unknown: number;
  // dsp.peek exposes byte knowledge, not partially known bit masks.
  knownByteMask: string;
}

async function bounded<T>(operation: Promise<T>, ms: number, label: string): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([
      operation,
      new Promise<never>((_, reject) => {
        timer = setTimeout(() => reject(new Error(`${label} exceeded ${ms} ms`)), ms);
      }),
    ]);
  } finally {
    clearTimeout(timer);
  }
}

function sha256(data: Buffer | string): string {
  return createHash('sha256').update(data).digest('hex');
}

interface ProofFile {
  path: string;
  sha256: string;
}
interface BootFixture {
  schema: 1;
  readyDir: string;
  baseSha256: string;
  patchedSyxSha256: string;
  dspPatch: ProofFile;
  packedDspImage: ProofFile;
  coldState: ProofFile & { pc: number; overlayBytes: number };
  coldIdle: ProofFile & { pc: number };
  originalReadyDspSnapshot: ProofFile;
  dspColdBinary: ProofFile;
  preparation: {
    command: string[];
    environment: { DSP_PATCH: string; DSP_DEVICE: string };
    maxInstructions: number;
    wallTimeoutSeconds: number;
    idleReached: boolean;
    log: ProofFile;
  };
  preExchange: {
    descriptors: { rootHostByteAddress: number; xcntHostByteAddress: number; xcnt: number; knownMask: number }[];
  };
}

/** Read-only validation: the parent prepares this fixture explicitly. No build,
 * DSP execution, descriptor resizing or snapshot preparation happens here. */
function readBootFixture(): BootFixture {
  const need = (ok: unknown, message: string) => {
    if (!ok) throw new Error(`LP0 0.5 boot fixture: ${message}`);
  };
  const file = process.env.DIGI_LP0_BOOT_FIXTURE;
  need(file && path.isAbsolute(file), 'set DIGI_LP0_BOOT_FIXTURE to an absolute provenance JSON path');
  const fixture = JSON.parse(fs.readFileSync(file!, 'utf8')) as BootFixture;
  need(fixture.schema === 1, 'schema must be 1');
  need(path.isAbsolute(fixture.readyDir), 'readyDir must be absolute');
  need(
    process.env.DIGI_PK_READY_DN2_112 &&
      path.resolve(process.env.DIGI_PK_READY_DN2_112) === path.resolve(fixture.readyDir),
    'DIGI_PK_READY_DN2_112 must explicitly select the same private readyDir',
  );
  const checkFile = (record: ProofFile, label: string) => {
    need(
      record && path.isAbsolute(record.path) && /^[a-f0-9]{64}$/.test(record.sha256),
      `${label} needs absolute path and SHA256`,
    );
    need(sha256(fs.readFileSync(record.path)) === record.sha256, `${label} hash mismatch`);
  };
  for (const [label, record] of Object.entries({
    dspPatch: fixture.dspPatch,
    packedDspImage: fixture.packedDspImage,
    coldState: fixture.coldState,
    coldIdle: fixture.coldIdle,
    originalReadyDspSnapshot: fixture.originalReadyDspSnapshot,
    dspColdBinary: fixture.dspColdBinary,
    preparationLog: fixture.preparation.log,
  }))
    checkFile(record, label);
  const inputs = resolveInputs('dn2', '1.12');
  need(
    path.dirname(fs.realpathSync(inputs.dspImage)) === fs.realpathSync(fixture.readyDir),
    'packed DSP image must resolve inside private readyDir: the daemon selects cold-idle beside its canonical path',
  );
  need(sha256(fs.readFileSync(inputs.syx)) === fixture.baseSha256, 'stock base hash mismatch');
  need(/^[a-f0-9]{64}$/.test(fixture.patchedSyxSha256), 'patchedSyxSha256 missing');
  need(
    path.resolve(fixture.coldIdle.path) === path.join(path.resolve(fixture.readyDir), 'cold-idle.dsp'),
    'coldIdle must name private readyDir/cold-idle.dsp',
  );
  need(
    sha256(fs.readFileSync(inputs.dspImage)) === fixture.packedDspImage.sha256,
    'private packed image must remain the original stock image',
  );
  need(
    sha256(fs.readFileSync(inputs.dspState)) === fixture.originalReadyDspSnapshot.sha256,
    'initial Session restore must retain the original ready DSP snapshot',
  );
  need(
    fixture.coldState.pc === 0x1c12e2 && fixture.coldState.overlayBytes === 0,
    'pre-init cold.state evidence must show PC1c12e2 and empty overlay',
  );
  need(
    fixture.coldIdle.pc >= 0xb88a49 && fixture.coldIdle.pc <= 0xb88abb,
    'cold-idle PC outside current DN2 idle range',
  );
  need(fixture.preparation.idleReached === true, 'dsp_cold exit alone does not prove idle');
  need(
    Number.isSafeInteger(fixture.preparation.maxInstructions) &&
      fixture.preparation.maxInstructions > 0 &&
      fixture.preparation.maxInstructions <= 800000000,
    'preparation needs an explicit instruction limit <=800M',
  );
  need(
    fixture.preparation.wallTimeoutSeconds > 0 && fixture.preparation.wallTimeoutSeconds <= 120,
    'preparation needs a wall timeout <=120 seconds',
  );
  need(
    fixture.preparation.environment.DSP_PATCH === fixture.dspPatch.path &&
      fixture.preparation.environment.DSP_DEVICE === 'dn2',
    'preparation DSP_PATCH/device mismatch',
  );
  for (const argument of [
    fixture.dspColdBinary.path,
    fixture.packedDspImage.path,
    fixture.coldState.path,
    fixture.coldIdle.path,
    String(fixture.preparation.maxInstructions),
  ]) {
    need(fixture.preparation.command.includes(argument), `preparation command lacks ${argument}`);
  }
  const idle = /^idle: pc=(0x[0-9a-f]+) after ([0-9]+) instructions$/m.exec(
    fs.readFileSync(fixture.preparation.log.path, 'utf8'),
  );
  need(
    idle && Number(idle[1]) === fixture.coldIdle.pc && Number(idle[2]) <= fixture.preparation.maxInstructions,
    'preparation log must contain the matching actual idle PC within the instruction limit',
  );
  const roots = [0x282c2960, 0x282c297c, 0x282c2998, 0x282c29b4];
  need(fixture.preExchange.descriptors.length === 4, 'exactly four pre-exchange descriptor observations required');
  for (const root of roots) {
    const entries = fixture.preExchange.descriptors.filter((entry) => entry.rootHostByteAddress === root);
    need(
      entries.length === 1 &&
        entries[0]!.xcntHostByteAddress === root + 0xc &&
        entries[0]!.xcnt === 0x566 &&
        entries[0]!.knownMask === 0xffffffff,
      `fully-known pre-exchange XCNT566 missing for descriptor0x${root.toString(16)}`,
    );
  }
  return fixture;
}
const bootFixture = enabled ? readBootFixture() : undefined;

// Full-bank execution requires an additional opt-in; both cases remain selectable
// by name and each uses its own fresh Session through this shared harness.
const cases = [
  {
    closeControl: false,
    fullBank: false,
    stockAudio: false,
    label: 'first-page',
    name: 'delivers one 4 KiB page through the real driver and rearms',
    bodyMs: 210000,
    testMs: 240000,
  },
  {
    fullBank: true,
    stockAudio: false,
    closeControl: false,
    label: 'full-bank',
    name: 'delivers full bank: 235 packets and 960000 bytes and remains armed for control',
    bodyMs: 240000,
    testMs: 300000,
  },
  {
    fullBank: true,
    stockAudio: true,
    closeControl: false,
    label: 'full-bank-stock-audio',
    name: 'delivers full bank with held stock audio before during and after transfer',
    bodyMs: 270000,
    testMs: 330000,
  },
  {
    fullBank: true,
    stockAudio: true,
    closeControl: true,
    label: 'close-control-stock-audio',
    name: 'closes diagnostic reader control over real LP0 and drains without resend with held stock audio',
    bodyMs: 300000,
    testMs: 360000,
  },
  {
    fullBank: true,
    stockAudio: false,
    closeControl: false,
    label: 'actual-two-track-voices',
    name: 'closes actual A SDK reader while independent B SDK reader continues',
    bodyMs: 330000,
    testMs: 390000,
  },
] as const;
for (const scenario of cases) {
  const { fullBank, stockAudio, closeControl } = scenario;
  const voices = scenario.label === 'actual-two-track-voices';
  const validBytes = fullBank ? BANK_BYTES : PAGE_BYTES;
  const packets = Math.ceil(validBytes / PAGE_BYTES);
  // Explicit opt-in: importing this file never builds or starts a native case.
  describe.skipIf(
    !enabled ||
      (voices
        ? process.env.DIGI_LP0_VOICES !== '1'
        : closeControl
          ? process.env.DIGI_LP0_CLOSE !== '1'
          : stockAudio
            ? process.env.DIGI_LP0_AUDIO !== '1'
            : fullBank && process.env.DIGI_LP0_FULL_BANK !== '1'),
  )(`DN2 1.12 private LP0 ${scenario.label} proof`, () => {
    const runDir = path.join(root, 'out/runs/lp0-proof', randomUUID());
    if (enabled) fs.mkdirSync(runDir, { recursive: true });
    const handle = withSession({ runDir, device: 'dn2', firmware: '1.12', dsp: 'sharc', binary, restart: false });

    test(
      scenario.name,
      async () => {
        const session = handle.session;
        let plan: RuntimePlan | undefined;
        let phase = 'prepare';
        let stopped = false;
        const observations: Record<string, unknown> = {};
        const save = (name: string, value: unknown) =>
          fs.writeFileSync(path.join(runDir, name), JSON.stringify(value, null, 2));
        // The stock CF sender can wait indefinitely. Every RPC and the complete body
        // have wall deadlines; finally closes only this session's owned daemon.
        const rpc = async <T>(method: string, params: object = {}, timeout = 10000): Promise<T> => {
          if (stopped) throw new Error('LP0 proof has stopped');
          const reply = await bounded(session.client.call<T>(method, params), timeout, method);
          if (stopped) throw new Error('LP0 proof has stopped');
          return reply;
        };
        const audioPath = path.join(runDir, 'stock-held.wav');
        const audioWindows: Partial<Record<'before' | 'during' | 'after', [number, number]>> = {};
        const AUDIO_MIN_WINDOW = 48000; // Interleaved values: half a second of stereo48k.
        const AUDIO_MAX_SAMPLES = 120 * 96000; // Bound successful capture artifacts to two audio minutes.
        let noteHeld = false;
        let capturing = false;
        let lastCaptureCount = 0;
        const audioMarker = async (label: string) => {
          const tap = await rpc<{ samples: number; non_finite: number; nonzero: number; peak: number; rms: number }>(
            'pcm.tap',
            { action: 'report' },
          );
          // capture_count is the WAV's own origin. Tap start precedes capture,
          // so its cumulative samples must never be used as file offsets.
          const { audio } = await rpc<{ audio: { pcm_capture_count: number; pcm_capture_error: string | null } }>(
            'audio.report',
          );
          const count = audio.pcm_capture_count;
          expect(audio.pcm_capture_error).toBeNull();
          expect(Number.isSafeInteger(count)).toBe(true);
          expect(count % 2).toBe(0);
          expect(count).toBeGreaterThanOrEqual(lastCaptureCount);
          expect(count).toBeLessThanOrEqual(AUDIO_MAX_SAMPLES);
          lastCaptureCount = count;
          fs.appendFileSync(
            path.join(runDir, 'stock-audio-timing.jsonl'),
            JSON.stringify({
              label,
              captureSamples: count,
              tapSamples: tap.samples,
              nonFinite: tap.non_finite,
              nonzero: tap.nonzero,
              peak: tap.peak,
              rms: tap.rms,
              wallMs: Date.now(),
            }) + '\n',
          );
          return count;
        };
        const cleanupAudio = async () => {
          const results: Record<string, unknown> = {};
          if (noteHeld) {
            try {
              results.release = await bounded(
                session.client.inputButton(button('TRIG 1'), false),
                2000,
                'release stock note',
              );
            } catch (error) {
              results.release = { error: String(error) };
            }
            noteHeld = false;
          }
          if (capturing) {
            try {
              results.captureStop = await bounded(
                session.client.call('pcm.tap', { action: 'capture_stop' }),
                2000,
                'stop stock PCM capture',
              );
            } catch (error) {
              results.captureStop = { error: String(error) };
            }
            capturing = false;
          }
          if (Object.keys(results).length) save('stock-audio-cleanup.json', results);
        };
        const startStockAudio = async () => {
          phase = 'stock held-note baseline';
          await rpc('pcm.tap', { action: 'start' });
          capturing = true; // Cleanup also covers a capture request whose reply times out.
          await rpc('pcm.tap', { action: 'capture', path: audioPath });
          noteHeld = true;
          await rpc('input.button', { code: button('TRIG 1'), down: true });
          const start = await audioMarker('before-start; stock TRIG1 held, no retriggers');
          await expect
            .poll(() => audioMarker('before-poll'), { timeout: 30000, interval: 50 })
            .toBeGreaterThanOrEqual(start + AUDIO_MIN_WINDOW);
          audioWindows.before = [start, await audioMarker('before-end')];
        };
        const finishStockAudio = async () => {
          phase = 'stock held-note after transfer';
          const start = await audioMarker('after-start; all235 callbacks complete');
          await expect
            .poll(() => audioMarker('after-poll'), { timeout: 30000, interval: 50 })
            .toBeGreaterThanOrEqual(start + AUDIO_MIN_WINDOW);
          audioWindows.after = [start, await audioMarker('after-end')];
          await cleanupAudio();
          const wav = fs.readFileSync(audioPath);
          expect(wav.length).toBeGreaterThanOrEqual(44);
          expect(wav.toString('ascii', 0, 4)).toBe('RIFF');
          expect(wav.readUInt32LE(4) + 8).toBe(wav.length);
          expect(wav.toString('ascii', 8, 16)).toBe('WAVEfmt ');
          expect(wav.readUInt32LE(16)).toBe(16);
          expect(wav.readUInt16LE(20), 'IEEE float WAV').toBe(3);
          expect(wav.readUInt16LE(22)).toBe(2);
          expect(wav.readUInt32LE(24)).toBe(48000);
          expect(wav.readUInt16LE(32)).toBe(8);
          expect(wav.readUInt16LE(34)).toBe(32);
          expect(wav.toString('ascii', 36, 40)).toBe('data');
          expect(wav.readUInt32LE(40)).toBe(wav.length - 44);
          expect((wav.length - 44) % 8).toBe(0);
          const samples = (wav.length - 44) / 4;
          expect(samples).toBeGreaterThanOrEqual(lastCaptureCount);
          expect(samples).toBeLessThanOrEqual(AUDIO_MAX_SAMPLES);
          const metrics: Record<string, unknown> = {};
          for (const name of ['before', 'during', 'after'] as const) {
            const range = audioWindows[name];
            expect(range, `${name} has capture-origin sample offsets`).toBeDefined();
            const [lo, hi] = range!;
            expect(lo % 2).toBe(0);
            expect(hi % 2).toBe(0);
            expect(lo).toBeGreaterThanOrEqual(0);
            expect(hi).toBeLessThanOrEqual(samples);
            expect(hi - lo).toBeGreaterThanOrEqual(AUDIO_MIN_WINDOW);
            let finite = true,
              nonzero = 0,
              peak = 0,
              squareSum = 0,
              zeroRun = 0,
              longestZeroRun = 0;
            const windows: { start: number; samples: number; peak: number; rms: number; nonzero: number }[] = [];
            for (let from = lo; from < hi; from += 9600) {
              const end = Math.min(from + 9600, hi);
              let windowPeak = 0,
                windowSq = 0,
                windowNonzero = 0;
              for (let i = from; i < end; i++) {
                const value = wav.readFloatLE(44 + i * 4);
                finite &&= Number.isFinite(value);
                if (value !== 0) {
                  nonzero++;
                  windowNonzero++;
                  zeroRun = 0;
                } else {
                  zeroRun++;
                  longestZeroRun = Math.max(longestZeroRun, zeroRun);
                }
                peak = Math.max(peak, Math.abs(value));
                windowPeak = Math.max(windowPeak, Math.abs(value));
                squareSum += value * value;
                windowSq += value * value;
              }
              windows.push({
                start: from,
                samples: end - from,
                peak: windowPeak,
                rms: Math.sqrt(windowSq / (end - from)),
                nonzero: windowNonzero,
              });
            }
            metrics[name] = {
              sampleRange: [lo, hi],
              samples: hi - lo,
              finite,
              nonzero,
              peak,
              rms: Math.sqrt(squareSum / (hi - lo)),
              longestExactZeroRunSamples: longestZeroRun,
              windows,
            };
          }
          const evidence = {
            path: audioPath,
            sha256: sha256(wav),
            sampleRate: 48000,
            channels: 2,
            samples,
            source:
              'Stock DN2 TRIG1(code25) held continuously; no machine selection, firmware adjustment or retrigger.',
            overlap:
              'during starts after first START publication and ends before final START publication; capture-file sample counters, not frame counts.',
            limitation:
              'Finite/non-silent sampled windows only; 100ms metrics record quiet intervals. No dropout-free, performance or hardware claim.',
            metrics,
          };
          save('stock-audio-metrics.json', evidence);
          observations.stockAudio = evidence;
          for (const name of ['before', 'during', 'after'] as const) {
            const metric = metrics[name] as { finite: boolean; nonzero: number; peak: number; rms: number };
            expect(metric.finite, `${name} stock PCM is finite`).toBe(true);
            expect(
              metric.nonzero,
              `${name} held stock note is non-silent; report natural decay rather than adjusting firmware`,
            ).toBeGreaterThan(0);
            expect(metric.peak).toBeGreaterThan(1e-6);
            expect(metric.rms).toBeGreaterThan(1e-8);
          }
        };
        const raw = async (addr: number, len: number, diagnostic = false): Promise<RawMemory> => {
          const result = diagnostic
            ? await bounded(
                session.client.call<{ hex: string; unknown: number }>('dsp.peek', { addr, len }),
                2000,
                'diagnostic dsp.peek',
              )
            : await rpc<{ hex: string; unknown: number }>('dsp.peek', { addr, len });
          expect(result.hex.length).toBe(len * 2);
          expect(result.hex).toMatch(/^(?:[0-9a-fA-F]{2}|\?\?)+$/);
          return {
            addr,
            len,
            // RPC elapsed/icount metadata changes between read-only observations.
            hex: result.hex,
            unknown: result.unknown,
            knownByteMask: result.hex
              .match(/../g)!
              .map((byte) => (byte === '??' ? '00' : 'ff'))
              .join(''),
          };
        };
        const bytes = async (addr: number, len: number): Promise<Buffer> => {
          const result = await raw(addr, len);
          expect(result.unknown, `fully known bytes at 0x${addr.toString(16)}`).toBe(0);
          return Buffer.from(result.hex, 'hex');
        };
        const word = async (addr: number) => (await bytes(addr, 4)).readUInt32LE();
        const symbol = (name: string) => planSymbol(plan!, name);
        const cfWord = async (addr: number) => {
          const { value } = await rpc<{ value: number | null }>('mem.cf.peek', { addr });
          expect(value, `known CF word at 0x${addr.toString(16)}`).not.toBeNull();
          return value!;
        };
        const tail = async () => {
          const chunks: RawMemory[] = [];
          for (let offset = PAGE_BYTES; offset < BANK_BYTES; offset += 65536) {
            chunks.push(await raw(BASE + offset, Math.min(65536, BANK_BYTES - offset)));
          }
          return chunks;
        };
        const frames = async () => {
          const status = await rpc<{ dsp_frames: number }>('status');
          expect(Number.isSafeInteger(status.dsp_frames)).toBe(true);
          return status.dsp_frames;
        };
        const cfAck = async () => {
          const address = symbol('lp_ack');
          const sequence = await cfWord(address);
          const [valid, status, error, pages, bytes] = await Promise.all(
            [4, 8, 12, 16, 20].map((offset) => cfWord(address + offset)),
          );
          const after = await cfWord(address);
          return { sequence, valid, status, error, pages, bytes, coherent: !(sequence & 1) && sequence === after };
        };
        const expectAck = async (pages: number, bytes: number, status: number) => {
          await expect
            .poll(
              async () => {
                const ack = await cfAck();
                observations.cfAck = ack;
                return (
                  ack.coherent &&
                  ack.valid === 1 &&
                  ack.status === status &&
                  ack.error === 0 &&
                  ack.pages === pages &&
                  ack.bytes === bytes
                );
              },
              { timeout: 20000, interval: 20 },
            )
            .toBe(true);
          return cfAck();
        };
        const armed = async () => {
          const [cfg, stat, addr, count, modify] = await Promise.all([
            word(DMA + 8),
            word(DMA + 0x30),
            word(DMA + 4),
            word(DMA + 0xc),
            word(DMA + 0x10),
          ]);
          return {
            cfg,
            stat,
            addr,
            count,
            modify,
            ready: (cfg & 1) !== 0 && ((cfg >>> 12) & 7) === 0 && (stat & 0x700) !== 0,
          };
        };
        const diagnostics = async () => {
          const capture: Record<string, unknown> = {
            phase,
            observations,
            runDir,
            binary,
            patchkit,
            paths: {
              daemonStderr: path.join(runDir, 'daemon.stderr'),
              discovery: path.join(runDir, 'current.json'),
              plan: path.join(runDir, 'lp0-proof.plan.json'),
            },
          };
          const attempt = async (name: string, operation: () => Promise<unknown>) => {
            try {
              capture[name] = await bounded(operation(), 2000, `diagnostic ${name}`);
            } catch (error) {
              capture[name] = { error: String(error) };
            }
          };
          await Promise.all([
            attempt('status', () => session.status()),
            attempt('mmio', () => raw(LP, 8, true)),
            attempt('dma', () => raw(DMA, 0x34, true)),
            attempt('port', () => raw(PORT, 0x20, true)),
            attempt('bankPage', async () => {
              const memory = await raw(BASE, PAGE_BYTES, true);
              const readback = memory.unknown === 0 ? Buffer.from(memory.hex, 'hex') : undefined;
              return {
                ...memory,
                sha256: readback ? sha256(readback) : null,
                head: memory.hex.slice(0, 128),
                tail: memory.hex.slice(-128),
              };
            }),
            ...(plan
              ? [
                  attempt('globals', async () =>
                    Object.fromEntries(
                      await Promise.all(globals.map(async (name) => [name, await raw(symbol(name), 4, true)])),
                    ),
                  ),
                  attempt('descriptor', async () => raw(await word(symbol('_lp_descriptor')), 28, true)),
                  attempt('savedHookContext', () => raw(symbol('_lp_save'), 292, true)),
                  attempt('cfAck', () => cfAck()),
                  attempt('mail', async () =>
                    Promise.all([0, 4, 8, 12].map((off) => session.client.memPeek(symbol('lp_mail') + off))),
                  ),
                  attempt('privateState', async () => {
                    const value = await raw(symbol('_lp_state'), 4, true);
                    const ptr = value.unknown === 0 ? Buffer.from(value.hex, 'hex').readUInt32LE() : 0;
                    return ptr >= 0x20000000 && ptr + 128 <= 0x20100000 ? raw(ptr, 128, true) : { pointer: ptr };
                  }),
                  attempt('rx', async () => {
                    const value = await raw(symbol('_lp_rx'), 4, true);
                    const ptr = value.unknown === 0 ? Buffer.from(value.hex, 'hex').readUInt32LE() : 0;
                    return ptr >= 0x20000000 && ptr + 4100 <= 0x20100000 ? raw(ptr, 4100, true) : { pointer: ptr };
                  }),
                ]
              : []),
          ]);
          save('diagnostics.json', capture);
        };

        const run = async () => {
          const inputs = resolveInputs('dn2', '1.12');
          const stockSha = sha256(fs.readFileSync(inputs.syx));
          save('inputs.json', { inputs, stockSha, binary, patchkit, bootFixture });
          save('stock-load.json', session.loadResult);
          // Refuse stale objects before apply, so this opt-in never invokes a build.
          const checked = await exec(
            patchkit!,
            ['--root', root, 'build', 'core/dsp-lp0-proof', 'core/expansion', '--check'],
            { cwd: root, timeout: 30000 },
          );
          fs.writeFileSync(path.join(runDir, 'object-check.log'), checked.stdout + checked.stderr);
          const output = path.join(runDir, 'lp0-proof.syx');
          const planFile = path.join(runDir, 'lp0-proof.plan.json');
          const applied = await exec(
            patchkit!,
            [
              '--root',
              root,
              'apply',
              '--firmware',
              inputs.syx,
              '--modules',
              'core/dsp-lp0-proof',
              '-o',
              output,
              '--plan',
              planFile,
            ],
            { cwd: root, timeout: 30000, maxBuffer: 4 * 1024 * 1024 },
          );
          fs.writeFileSync(path.join(runDir, 'patch.log'), applied.stdout + applied.stderr);
          plan = JSON.parse(fs.readFileSync(planFile, 'utf8')) as RuntimePlan;
          expect(plan.base).toMatchObject({ device: 'dn2', version: '1.12', syx_sha256: stockSha });
          expect(plan.modules.map((module) => module.split('@')[0]).sort()).toEqual([
            'core/dsp-lp0-proof',
            'core/dsp-voice',
            'core/expansion',
            'core/machines',
            'core/params',
          ]);
          expect(plan.output?.syx_sha256).toMatch(/^[a-f0-9]{64}$/);
          expect(plan.output?.syx_sha256, 'fixture must bind the exact current 0.5 patch').toBe(
            bootFixture!.patchedSyxSha256,
          );
          for (const name of [
            ...globals,
            '_lp_dma_storage',
            '_lp_descriptor',
            '_lp_dma_base',
            '_lp_dma_limit',
            '_lp_boot_status',
            '_lp_boot_error',
            '_lp_save',
            'lp_mail',
            'lp_ack',
            'lp_staging',
          ])
            symbol(name);
          phase = 'patched startup';
          const load = await rpc<{ syx_sha256: string }>(
            'firmware.load',
            { syx: output, base_sha256: stockSha, start: 'auto', auto_no: true },
            120000,
          );
          save('patched-load.json', load);
          expect(load.syx_sha256).toBe(plan.output?.syx_sha256);
          await rpc('run.mode', { mode: 'free' });
          await expect
            .poll(async () => (await rpc<{ cf: { ready: boolean } }>('status')).cf.ready, { timeout: 15000 })
            .toBe(true);
          const status = await session.status();
          save('patched-status.json', status);
          expect(status.firmware).toMatchObject({
            base_sha256: stockSha,
            syx_sha256: plan.output?.syx_sha256,
            injected: true,
          });
          await session.expectNoFault('LP0 setup');
          phase = 'CF-observed receiver setup';
          await expectAck(0, 0, 2);
          // DSP reads are independent oracles; completion is driven by the CF wire ACK.
          expect(await word(symbol('_lp_status'))).toBe(2);
          const wireCounts = {
            cfBytes: await cfWord(0x42441958),
            txCount: await word(0x3102d20c),
            rxCount: await word(0x3102d28c),
          };
          save('wire-after-boot.json', { ...wireCounts, preExchange: bootFixture!.preExchange });
          expect(wireCounts).toEqual({ cfBytes: 0xacc, txCount: 0x566, rxCount: 0x566 });
          expect(await word(symbol('_lp_error'))).toBe(0);
          expect(await word(symbol('_lp_pages'))).toBe(0);
          expect(await word(symbol('_lp_bytes'))).toBe(0);
          const rx = await word(symbol('_lp_rx'));
          const state = await word(symbol('_lp_state'));
          expect(rx & 63).toBe(0);
          const dmaBase = await word(symbol('_lp_dma_base'));
          const dmaLimit = await word(symbol('_lp_dma_limit'));
          expect(await word(symbol('_lp_boot_status'))).toBe(1);
          expect(await word(symbol('_lp_boot_error'))).toBe(0);
          expect(dmaBase & 63).toBe(0);
          expect(dmaLimit).toBe(dmaBase + 4352);
          expect(dmaBase).toBeGreaterThanOrEqual(symbol('_lp_dma_storage'));
          expect(dmaLimit).toBeLessThanOrEqual(symbol('_lp_dma_storage') + 4416);
          expect(await word(symbol('_lp_descriptor'))).toBe(dmaBase);
          expect(state).toBe(dmaBase + 64);
          expect(rx).toBe(dmaBase + 192);
          const cache = await Promise.all(
            [0x3e018, 0x3e019, 0x3e002].map((addr) =>
              rpc<{ addr: number; value: number | null }>('dsp.peek_word', { addr }),
            ),
          );
          save('cache-window.json', { dmaBase, dmaLimit, cache });
          expect(cache[0]).toMatchObject({ addr: 0x3e018, value: dmaBase });
          expect(cache[1]).toMatchObject({ addr: 0x3e019, value: dmaLimit });
          expect(cache[2]).toMatchObject({ addr: 0x3e002 });
          expect(cache[2]!.value).not.toBeNull();
          expect(cache[2]!.value! & 0x300).toBe(0x300);
          for (const [start, size] of [
            [await word(symbol('_lp_descriptor')), 28] as const,
            [symbol('_lp_save'), 292] as const,
            [state, 128] as const,
            ...globals.map((name) => [symbol(name), 4] as const),
          ]) {
            expect(start + size <= rx || rx + 4160 <= start, 'RX cache lines exclude other owners').toBe(true);
          }
          expect(symbol('_lp_dma_storage')).toBeGreaterThanOrEqual(0x20000000);
          expect(rx + 4160).toBeLessThanOrEqual(0x20100000);
          expect(await word(PORT + 0x14)).toBe(state);
          expect(await word(symbol('_lp_handle'))).toBe(0x268ca8);
          const canary = Buffer.alloc(4096);
          for (let i = 0; i < canary.length; i += 4) canary.writeUInt32LE(CANARY, i);
          const guardAddresses = [BASE - 4096, BASE + BANK_BYTES];
          const guardBeforeRaw = await Promise.all(guardAddresses.map((addr) => raw(addr, 4096)));
          save('guards-before.json', guardBeforeRaw);
          const knownGuards = (chunks: RawMemory[]) =>
            chunks.map((chunk) => {
              expect(chunk.unknown, 'full canary bytes are known').toBe(0);
              return Buffer.from(chunk.hex, 'hex');
            });
          const guardBefore = knownGuards(guardBeforeRaw);
          expect(guardBefore).toEqual([canary, canary]);
          // The first-page proof preserves unknown markers in the untouched tail.
          // Unknown bytes remaining unknown do not establish their concrete value.
          const tailBefore = fullBank ? undefined : await tail();
          if (tailBefore) save('tail-before.json', tailBefore);
          const expected = Buffer.alloc(validBytes);
          const pcmA = (frame: number) => {
            const n = frame % 240;
            return Math.round((n < 120 ? -1 + n / 60 : 3 - n / 60) * 8192);
          };
          const pattern = (index: number) =>
            voices
              ? ((pcmA(index * 2) & 0xffff) | ((pcmA(index * 2 + 1) & 0xffff) << 16)) >>> 0
              : (Math.imul(index, 0x01010101) ^ 0x80ff3c5a) >>> 0;
          // Multiplication by an odd word and XOR are bijections: all source words
          // have unique values, including the unused padding of the final packet.
          for (let i = 0; i < expected.length / 4; i++) expected.writeUInt32LE(pattern(i), i * 4);
          fs.writeFileSync(path.join(runDir, 'expected-le.bin'), expected);
          const mail = symbol('lp_mail');
          expect(await cfWord(mail)).toBe(0);
          await expect.poll(async () => (await armed()).ready, { timeout: 10000 }).toBe(true);
          const before = await armed();
          expect(before).toMatchObject({ ready: true, addr: rx, count: 0x401, modify: 4 });
          const frameBefore = await frames();
          observations.before = { dma: before, rx, state, frameBefore, packets, validBytes };
          save('before-send.json', observations.before);
          if (stockAudio) await startStockAudio();
          const staged = Buffer.alloc(PAGE_BYTES);
          let transferAudioStart: number | undefined;
          for (let tag = 0; tag < packets; tag++) {
            phase = `retained CF request ${tag}/${packets - 1}`;
            const progressBefore = Math.min(tag * PAGE_BYTES, validBytes);
            const progressAfter = Math.min((tag + 1) * PAGE_BYTES, validBytes);
            expect(await cfWord(mail)).toBe(tag === 0 ? 0 : 3);
            expect(await cfWord(mail + 12)).toBe(tag);
            await expectAck(tag, progressBefore, 2);
            expect(await word(symbol('_lp_status'))).toBe(2);
            expect(await word(symbol('_lp_error'))).toBe(0);
            expect(await word(symbol('_lp_pages'))).toBe(tag);
            expect(await word(symbol('_lp_bytes'))).toBe(progressBefore);
            await expect.poll(async () => (await armed()).ready, { timeout: 10000 }).toBe(true);
            for (let i = 0; i < 1024; i++) staged.writeUInt32BE(pattern(tag * 1024 + i), i * 4);
            if (tag === 0 || tag === packets - 1) {
              fs.writeFileSync(
                path.join(runDir, fullBank ? `staging-${tag === 0 ? 'first' : 'last'}-be.bin` : 'staging-be.bin'),
                staged,
              );
            }
            // One retained request: staging is reused only after BOTH sender return
            // and exact callback progress for the previous request have succeeded.
            await rpc('mem.cf.write', { addr: symbol('lp_staging'), hex: staged.toString('hex') });
            await rpc('mem.cf.poke', { addr: mail + 8, value: tag });
            expect(await word(symbol('_lp_pages'))).toBe(tag);
            expect(await word(symbol('_lp_bytes'))).toBe(progressBefore);
            const publishGate = await armed();
            expect(publishGate).toMatchObject({ ready: true, addr: rx, count: 0x401, modify: 4 });
            observations.publishGate = { tag, ...publishGate };
            if (tag === 0) save('publish-gate.json', observations.publishGate);
            fs.appendFileSync(
              path.join(runDir, 'requests.jsonl'),
              JSON.stringify({
                event: 'publish',
                tag,
                validBytes: progressAfter - progressBefore,
                stagedSha256: sha256(staged),
                dma: publishGate,
              }) + '\n',
            );
            if (stockAudio && tag === packets - 1) {
              const end = await audioMarker('during-end; before last START');
              expect(transferAudioStart).toBeDefined();
              audioWindows.during = [transferAudioStart!, end];
            }
            await rpc('mem.cf.poke', { addr: mail, value: 1 }); // START is published last.
            if (stockAudio && tag === 0) transferAudioStart = await audioMarker('during-start; after first START');
            await expect.poll(() => cfWord(mail), { timeout: 20000, interval: 20 }).toBe(3);
            expect(await cfWord(mail + 4)).toBe(0);
            expect(await cfWord(mail + 12)).toBe(tag + 1);
            phase = `CF completion ${tag}/${packets - 1}`;
            const terminal = fullBank && tag === packets - 1;
            const ack = await expectAck(tag + 1, progressAfter, terminal ? 3 : 2);
            expect(await word(symbol('_lp_pages'))).toBe(tag + 1);
            expect(await word(symbol('_lp_bytes'))).toBe(progressAfter);
            expect(await word(symbol('_lp_error'))).toBe(0);
            expect(await word(symbol('_lp_status'))).toBe(terminal ? 3 : 2);
            const dma = await armed();
            expect(dma).toMatchObject({ ready: true, addr: rx, count: 0x401, modify: 4 });
            const captureSamples = stockAudio ? await audioMarker(`packet-${tag}-consumed`) : undefined;
            const progress = {
              tag,
              ack,
              pages: tag + 1,
              bytes: progressAfter,
              dma,
              ...(stockAudio ? { captureSamples } : {}),
            };
            observations.lastProgress = progress;
            fs.appendFileSync(
              path.join(runDir, 'requests.jsonl'),
              JSON.stringify({ event: 'consumed', ...progress }) + '\n',
            );
          }
          if (voices) {
            phase = 'actual two-track source dispatch pilot';
            type Registry = {
              name: string;
              addr: number;
              first: number;
              row_size: number;
              fields: [string, string, number][];
              entries: { id: string; index: number; module: string }[];
            };
            const machines = (plan as RuntimePlan & { registries: Registry[] }).registries.find(
              (registry) => registry.name === 'machines',
            )!;
            const machine = machines.entries.find((entry) => entry.id === 'lp_sampler')!;
            expect(machine.module.split('@')[0]).toBe('core/dsp-lp0-proof');
            const offset = machines.fields.find(([name]) => name === 'position')![2];
            const position = await cfWord(
              machines.addr + (machine.index - machines.first) * machines.row_size + offset,
            );
            expect(position).toBeLessThan(16);
            const clock = async () => {
              const elapsed = (await rpc<{ elapsed: number }>('status')).elapsed;
              if (typeof elapsed !== 'number') throw new Error('panel requires elapsed instruction clock');
              return elapsed;
            };
            const until = async (target: number) => {
              await expect.poll(clock, { timeout: 20000, interval: 25 }).toBeGreaterThanOrEqual(target);
            };
            const settle = async (ms: number) => until((await clock()) + ms * 132000);
            const tap = async (name: string) => {
              const reply = await rpc<{ release_at: number }>('input.tap', { code: button(name), hold: 20 * 132000 });
              expect(typeof reply.release_at).toBe('number');
              await until(reply.release_at + 5 * 132000);
            };
            const voiceCleanup: Record<string, unknown> = {};
            const releaseButton = async (name: string) => {
              try {
                voiceCleanup[name] = await bounded(
                  session.client.call('input.button', { code: button(name), down: false }),
                  2000,
                  `release ${name}`,
                );
              } catch (error) {
                voiceCleanup[name] = { error: String(error) };
              }
            };
            const track = async (n: number) => {
              try {
                await rpc('input.button', { code: button('TRK'), down: true });
                await settle(5);
                await tap(`TRIG ${n}`);
              } finally {
                await releaseButton('TRK');
              }
              await settle(100);
            };
            const select = async (n: number) => {
              await tap('NO');
              await track(n);
              try {
                await rpc('input.button', { code: button('FUNC'), down: true });
                await settle(5);
                await tap('SRC');
              } finally {
                await releaseButton('FUNC');
              }
              await settle(200);
              const menu: number[] = [];
              for (let i = 0; i < machines.first + machines.entries.length; i++)
                menu.push(await cfWord(symbol('mf_list_buf') + i * 4));
              expect(menu[position], 'live menu position must identify private machine').toBe(machine.index);
              save(`voices-track${n}-menu.json`, { machine, position, menu });
              for (let i = 0; i < 16; i++) {
                await tap('UP');
                await settle(80);
              }
              for (let i = 0; i < position; i++) {
                await tap('DOWN');
                await settle(80);
              }
              await tap('YES');
              await settle(300);
              await tap('YES');
              await settle(300);
              save(`voices-track${n}-page.json`, await rpc('framebuffer.get', { hash: true, data: true }));
            };
            const counter = (t: string, name: string) => word(symbol(`_lp_reader_${t}_${name}`));
            const callbackSnapshot = async (track: string, afterSequence = 0) => {
              const sequenceAddress = symbol(`_lp_reader_${track}_snapshot_seq`);
              expect(symbol(`_lp_reader_${track}_snapshot`)).toBe(sequenceAddress + 4);
              let result: { sequence: number; hex: string; values: number[] } | undefined;
              await expect
                .poll(
                  async () => {
                    // Coupled Peek runs synchronously on the DSP worker without
                    // guest execution inside peek_bytes (sharc_peer.rs).
                    const observation = await bytes(sequenceAddress, 132);
                    const sequence = observation.readUInt32LE(0);
                    if (sequence <= afterSequence || sequence & 1) return false;
                    const block = observation.subarray(4);
                    result = {
                      sequence,
                      hex: block.toString('hex'),
                      values: Array.from({ length: 32 }, (_, i) => block.readFloatLE(i * 4)),
                    };
                    return true;
                  },
                  { timeout: 15000, interval: 20 },
                )
                .toBe(true);
              return result!;
            };
            const snapshot = async () => ({
              a: (await bytes(symbol('_lp_close_a_open'), 24)).toString('hex'),
              b: (await bytes(symbol('_lp_close_b_open'), 24)).toString('hex'),
              aReads: await counter('a', 'reads'),
              bReads: await counter('b', 'reads'),
              aNotes: await counter('a', 'notes'),
              bNotes: await counter('b', 'notes'),
              aSnapshotSequence: await counter('a', 'snapshot_seq'),
              bSnapshotSequence: await counter('b', 'snapshot_seq'),
              mail: await Promise.all([0, 4, 8, 12, 16, 20].map((off) => cfWord(symbol('lp_close_mail') + off))),
              ack: await Promise.all([0, 4, 8, 12, 16, 20].map((off) => cfWord(symbol('lp_close_ack') + off))),
            });
            await select(1);
            await select(2);
            await track(1);
            const held = new Set<number>();
            let voiceCapture = false;
            try {
              held.add(1);
              await rpc('input.button', { code: button('TRIG 1'), down: true });
              await expect.poll(() => counter('a', 'notes'), { timeout: 15000 }).toBeGreaterThan(0);
              // Keep A held while switching source track and starting B. Failure
              // here stops the pilot; no firmware control words are fabricated.
              await track(2);
              held.add(2);
              await rpc('input.button', { code: button('TRIG 2'), down: true });
              await expect.poll(() => counter('b', 'notes'), { timeout: 15000 }).toBeGreaterThan(0);
              const pilot = await snapshot();
              await expect
                .poll(
                  async () =>
                    (await counter('a', 'reads')) > pilot.aReads && (await counter('b', 'reads')) > pilot.bReads,
                  { timeout: 15000 },
                )
                .toBe(true);
              const aState = await counter('a', 'state'),
                bState = await counter('b', 'state');
              expect(aState).not.toBe(bState);
              // The machine adapter supplies byte-addressed state/output pointers.
              const stateByte = (ptr: number) => ptr;
              expect(await word(stateByte(aState) + 32)).toBe(0);
              expect(await word(stateByte(bState) + 32)).toBe(1);
              const file = path.join(runDir, 'actual-voices.wav');
              await rpc('pcm.tap', { action: 'start' });
              voiceCapture = true;
              await rpc('pcm.tap', { action: 'capture', path: file });
              const start = await frames();
              await expect.poll(frames, { timeout: 15000 }).toBeGreaterThan(start + 160);
              const before = await snapshot();
              const captureSplit = (await rpc<{ audio: { pcm_capture_count: number } }>('audio.report')).audio
                .pcm_capture_count;
              save('voices-close-before.json', { ...before, captureSplit });
              const mail = symbol('lp_close_mail');
              for (const [off, value] of [
                [8, 1],
                [12, 0],
                [16, 1],
              ] as const)
                await rpc('mem.cf.poke', { addr: mail + off, value });
              await rpc('mem.cf.poke', { addr: mail, value: 1 });
              // PENDING may be missed when CLOSE lands between callback blocks.
              const intermediate = await snapshot();
              save('voices-close-observed.json', intermediate);
              await expect.poll(() => cfWord(mail), { timeout: 20000 }).toBe(3);
              const ack = symbol('lp_close_ack');
              await expect
                .poll(
                  async () => {
                    const sequence = await cfWord(ack);
                    const fields = await Promise.all([4, 8, 12, 16, 20].map((off) => cfWord(ack + off)));
                    return (
                      !(sequence & 1) &&
                      sequence === (await cfWord(ack)) &&
                      fields.every((value, i) => value === [1, 2, 1, 0, 1][i])
                    );
                  },
                  { timeout: 20000 },
                )
                .toBe(true);
              expect(await word(symbol('_lp_close_a_count'))).toBe(0);
              expect(await word(symbol('_lp_close_a_closed'))).toBe(1);
              expect(await word(symbol('_lp_close_a_ready'))).toBe(0);
              const drained = await snapshot();
              const postDrainCapture = (await rpc<{ audio: { pcm_capture_count: number } }>('audio.report')).audio
                .pcm_capture_count;
              save('voices-close-drained.json', { ...drained, captureSplit, postDrainCapture });
              await expect.poll(() => counter('b', 'reads'), { timeout: 15000 }).toBeGreaterThan(drained.bReads + 160);
              await expect
                .poll(
                  async () =>
                    (await rpc<{ audio: { pcm_capture_count: number } }>('audio.report')).audio.pcm_capture_count,
                  { timeout: 15000 },
                )
                .toBeGreaterThanOrEqual(postDrainCapture + 9600);
              expect(await counter('a', 'reads')).toBe(drained.aReads);
              for (const [field, value] of [
                ['open', 1],
                ['ready', 1],
                ['closed', 0],
                ['fault', 0],
                ['epoch', 1],
              ] as const)
                expect(await word(symbol(`_lp_close_b_${field}`))).toBe(value);
              expect(await word(stateByte(aState) + 20)).toBe(1);
              const aOut = await counter('a', 'output');
              // Stock stages mutate their output buffers after callback return.
              // Private copies observe our raw callback boundary coherently.
              const aRaw = await callbackSnapshot('a', before.aSnapshotSequence);
              expect(aRaw.hex).toBe(Buffer.alloc(128).toString('hex'));
              const bOut = await counter('b', 'output');
              expect(aOut).not.toBe(bOut);
              const bRaw = await callbackSnapshot('b', drained.bSnapshotSequence);
              const values = bRaw.values;
              expect(values.every(Number.isFinite)).toBe(true);
              expect(Math.max(...values.map(Math.abs))).toBeLessThanOrEqual(0.25);
              expect(values.some((value) => value !== 0)).toBe(true);
              for (let i = 1; i < values.length; i++)
                expect(Math.abs(values[i]! - values[i - 1]!)).toBeCloseTo(1 / 512, 6);
              await rpc('pcm.tap', { action: 'capture_stop' });
              voiceCapture = false;
              const wav = fs.readFileSync(file);
              const spectral = (lo: number, hi: number) => {
                let re = 0,
                  im = 0,
                  sq = 0;
                const n = Math.floor((hi - lo) / 2);
                for (let i = 0; i < n; i++) {
                  const value =
                    (wav.readFloatLE(44 + (lo + 2 * i) * 4) + wav.readFloatLE(44 + (lo + 2 * i + 1) * 4)) / 2;
                  expect(Number.isFinite(value)).toBe(true);
                  const angle = (2 * Math.PI * 93.75 * i) / 48000;
                  re += value * Math.cos(angle);
                  im += value * Math.sin(angle);
                  sq += value * value;
                }
                return { frames: n, component: (2 * Math.hypot(re, im)) / n, rms: Math.sqrt(sq / n) };
              };
              const total = (wav.length - 44) / 4;
              const component = { before: spectral(0, captureSplit), after: spectral(postDrainCapture, total) };
              for (const metric of Object.values(component)) {
                expect(metric.frames).toBeGreaterThan(2048);
                expect(metric.component).toBeGreaterThan(1e-7);
                expect(metric.component).toBeGreaterThan(metric.rms * 0.01);
              }
              save('voices-close-final.json', {
                component,
                captureSplit,
                postDrainCapture,
                pilot,
                before,
                drained,
                final: await snapshot(),
                aRaw,
                bRaw,
                outputPointers: { a: aOut, b: bOut, externalBuffersMayBeModifiedByStock: true },
                bBlock: values,
                file,
                sha256: sha256(fs.readFileSync(file)),
                actualAudioReader: true,
                limitation: 'Panel source-dispatch pilot and direct B block; mixed stock FX tails are retained.',
              });
            } finally {
              for (const n of held) await releaseButton(`TRIG ${n}`);
              if (voiceCapture) {
                try {
                  voiceCleanup.captureStop = await bounded(
                    session.client.call('pcm.tap', { action: 'capture_stop' }),
                    2000,
                    'stop actual voice capture',
                  );
                } catch (error) {
                  voiceCleanup.captureStop = { error: String(error) };
                }
              }
              save('voices-cleanup.json', voiceCleanup);
            }
          }
          if (closeControl) {
            phase = 'private diagnostic reader CLOSE, not an actual paused voice';
            const mail = symbol('lp_close_mail');
            const reply = symbol('lp_close_ack');
            const bBefore = await bytes(symbol('_lp_close_b_open'), 24);
            const injectCount = async (value: number, previous: number) => {
              const text = `BASE ${stockSha}\nDM ${symbol('_lp_close_a_count').toString(16)} ${value.toString(16).padStart(8, '0')} was ${previous.toString(16).padStart(8, '0')}\n`;
              save(`close-count-${value}.json`, { text, diagnosticFaultInjection: true });
              await rpc('dsp.patch', { text });
              expect(await word(symbol('_lp_close_a_count'))).toBe(value);
            };
            const exactClose = async (status: number) => {
              const before = await cfWord(reply);
              const fields = await Promise.all([4, 8, 12, 16, 20].map((offset) => cfWord(reply + offset)));
              const after = await cfWord(reply);
              observations.closeAck = { before, after, fields };
              return (
                !(before & 1) &&
                before === after &&
                fields.every((value, index) => value === [1, status, 1, 0, 1][index])
              );
            };
            await injectCount(1, 0);
            for (const [offset, value] of [
              [8, 1],
              [12, 0],
              [16, 1],
            ] as const)
              await rpc('mem.cf.poke', { addr: mail + offset, value });
            await rpc('mem.cf.poke', { addr: mail, value: 1 });
            await expect.poll(() => cfWord(mail), { timeout: 20000 }).toBe(2);
            await expect.poll(() => exactClose(1), { timeout: 20000 }).toBe(true);
            expect(await cfWord(mail + 20)).toBe(1);
            expect(await word(symbol('_lp_close_a_open'))).toBe(0);
            expect(await word(symbol('_lp_close_a_ready'))).toBe(0);
            expect(await word(symbol('_lp_close_a_closed'))).toBe(1);
            expect(await bytes(symbol('_lp_close_b_open'), 24)).toEqual(bBefore);
            const pendingFrame = await frames();
            await expect.poll(frames, { timeout: 10000 }).toBeGreaterThan(pendingFrame);
            expect(await cfWord(mail)).toBe(2);
            await injectCount(0, 1);
            await expect.poll(() => cfWord(mail), { timeout: 20000 }).toBe(3);
            await expect.poll(() => exactClose(2), { timeout: 20000 }).toBe(true);
            expect(await cfWord(mail + 4)).toBe(0);
            expect(await cfWord(mail + 20), 'drain poll must not resend CLOSE').toBe(1);
            expect(await bytes(symbol('_lp_close_b_open'), 24)).toEqual(bBefore);
            expect(await word(symbol('_lp_pages'))).toBe(235);
            expect(await word(symbol('_lp_bytes'))).toBe(BANK_BYTES);
            expect(await armed()).toMatchObject({ ready: true, addr: rx, count: 0x401, modify: 4 });
            save('close-control.json', {
              diagnosticReaderCountInjection: true,
              actualAudioReader: false,
              bBefore: bBefore.toString('hex'),
              ...observations,
            });
          }
          if (stockAudio) await finishStockAudio();
          const after = await armed();
          const frameAtCompletion = await frames();
          expect(frameAtCompletion).toBeGreaterThan(frameBefore);
          const readbackChunks: Buffer[] = [];
          for (let offset = 0; offset < validBytes; offset += 65536) {
            readbackChunks.push(await bytes(BASE + offset, Math.min(65536, validBytes - offset)));
          }
          const readback = Buffer.concat(readbackChunks);
          fs.writeFileSync(path.join(runDir, fullBank ? 'bank-full.bin' : 'bank-page.bin'), readback);
          save(fullBank ? 'readback-bank.json' : 'readback-page.json', {
            sha256: sha256(readback),
            expectedSha256: sha256(expected),
            bytes: validBytes,
            head: readback.subarray(0, 64).toString('hex'),
            tail: readback.subarray(-64).toString('hex'),
          });
          expect(readback).toEqual(expected);
          const guardAfterRaw = await Promise.all(guardAddresses.map((addr) => raw(addr, 4096)));
          save('guards-after.json', guardAfterRaw);
          const guardAfter = knownGuards(guardAfterRaw);
          expect(guardAfterRaw).toEqual(guardBeforeRaw);
          expect(guardAfter).toEqual(guardBefore);
          if (tailBefore) {
            const tailAfter = await tail();
            save('tail-after.json', tailAfter);
            expect(tailAfter).toEqual(tailBefore); // Includes unknown markers and byte-known masks.
          }
          phase = 'ordinary frame progress after transfer';
          await expect.poll(frames, { timeout: 10000 }).toBeGreaterThan(frameAtCompletion);
          const frameAfter = await frames();
          await session.expectNoFault(`LP0 ${scenario.label} and subsequent frames`);
          if (fullBank) {
            // Payload completion persists while the receiver remains armed for CLOSE.
            await expectAck(235, BANK_BYTES, 3);
            expect(await word(symbol('_lp_status'))).toBe(3);
            expect(await word(symbol('_lp_error'))).toBe(0);
            expect(await word(symbol('_lp_pages'))).toBe(235);
            expect(await word(symbol('_lp_bytes'))).toBe(BANK_BYTES);
            const terminalDma = await armed();
            expect(terminalDma).toMatchObject({ ready: true, addr: rx, count: 0x401, modify: 4 });
            expect(await cfWord(mail)).toBe(3);
            expect(await cfWord(mail + 12)).toBe(235);
            observations.terminalAfterFrames = { dma: terminalDma, pages: 235, bytes: BANK_BYTES, frameAfter };
            save('terminal-after-frames.json', observations.terminalAfterFrames);
          }
          observations.after = { dma: after, frameAtCompletion, frameAfter, pages: packets, bytes: validBytes };
          save('readback.json', {
            sha256: sha256(readback),
            expectedSha256: sha256(expected),
            head: readback.subarray(0, 64).toString('hex'),
            tail: readback.subarray(-64).toString('hex'),
            guardSha256: guardAfter.map(sha256),
            ...observations,
          });
          save('monitored-ranges.json', {
            unchanged: [
              { addr: BASE - 4096, bytes: 4096, unknown: 0 },
              ...(tailBefore
                ? [
                    {
                      addr: BASE + PAGE_BYTES,
                      bytes: BANK_BYTES - PAGE_BYTES,
                      unknown: tailBefore.reduce((sum, chunk) => sum + chunk.unknown, 0),
                    },
                  ]
                : []),
              { addr: BASE + BANK_BYTES, bytes: 4096, unknown: 0 },
            ],
            coverage:
              'Exact values and unknown-byte markers in these ranges only; unknown tail bytes remaining unknown do not establish concrete values. No universal external-write claim. dsp.peek does not expose partial bit masks.',
            frames: { before: frameBefore, atCompletion: frameAtCompletion, after: frameAfter },
            audioClaim: stockAudio
              ? 'Stock-note PCM finite/non-silent in before, interior-transfer and after windows; no dropout-free or performance claim.'
              : 'Ordinary DSP frames progress; no PCM amplitude or playback coexistence claim.',
            transferred: { pages: packets, bytes: validBytes },
          });
          if (voices) {
            phase = 'cached ready-pair restore';
            // Restore the boot-ready checkpoint, not the transferred sample bank.
            // A second initialization or patch application would erase boot state.
            // Loading the already active image is a no-op, so restore stock first.
            const stockRestore = await rpc<{ syx_sha256: string }>(
              'firmware.load',
              { syx: inputs.syx, start: 'snapshot', auto_no: true },
              30000,
            );
            expect(stockRestore.syx_sha256).toBe(stockSha);
            const restored = await rpc<{ cache_hit: boolean; start_used: string; syx_sha256: string }>(
              'firmware.load',
              { syx: output, base_sha256: stockSha, start: 'snapshot', auto_no: true },
              30000,
            );
            expect(restored).toMatchObject({
              cache_hit: true,
              start_used: 'snapshot-cache',
              syx_sha256: plan.output!.syx_sha256,
            });
            expect(await word(symbol('_lp_boot_status'))).toBe(1);
            expect(await word(symbol('_lp_boot_error'))).toBe(0);
            expect(await word(symbol('_lp_dma_base'))).toBe(dmaBase);
            expect(await word(symbol('_lp_dma_limit'))).toBe(dmaLimit);
            const restoredCache = await Promise.all(
              [0x3e018, 0x3e019, 0x3e002].map((addr) =>
                rpc<{ addr: number; value: number | null }>('dsp.peek_word', { addr }),
              ),
            );
            expect(restoredCache[0]).toMatchObject({ addr: 0x3e018, value: dmaBase });
            expect(restoredCache[1]).toMatchObject({ addr: 0x3e019, value: dmaLimit });
            expect(restoredCache[2]!.value).not.toBeNull();
            expect(restoredCache[2]!.value! & 0x300).toBe(0x300);
            const restoredFrame = await frames();
            await rpc('run.mode', { mode: 'free' });
            await expect.poll(frames, { timeout: 10000 }).toBeGreaterThan(restoredFrame);
            await session.expectNoFault('cached initialized DSP restore');
            save('cached-restore.json', { restored, dmaBase, dmaLimit, cache: restoredCache });
          }
        };

        try {
          await bounded(run(), scenario.bodyMs, `LP0 ${scenario.label} proof`);
          await diagnostics();
          save('result.json', { passed: true, runDir, scenario: scenario.label, pages: packets, bytes: validBytes });
        } catch (error) {
          stopped = true; // A timed-out body cannot resume and publish another write.
          save('result.json', { passed: false, phase, runDir, error: String(error) });
          await bounded(diagnostics(), 12000, 'failure diagnostics').catch((diagnosticError) =>
            save('diagnostic-error.json', { error: String(diagnosticError) }),
          );
          throw error;
        } finally {
          stopped = true;
          if (stockAudio) await cleanupAudio();
          await session.close();
        }
      },
      scenario.testMs,
    );
  });
}
