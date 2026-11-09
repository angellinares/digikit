import { be32, eventLine, hasCap, moduleNames, runtimeEvent, type DkLogEvent } from '@digi/dk-session';
import { CMD, LOG_KIND, SERVICE_CMD } from '@digi/dk-session/gen_proto';
import { expect } from 'vitest';
import type { PatchedSession } from '@digi/regression/support.ts';
import { waitFrames } from '@digi/regression/runtime/debug-kit.ts';
import { deviceProfile } from '@digi/regression/runtime/device-data.ts';
import { liveArtifacts } from '@digi/regression/runtime/artifacts.ts';
import { DEBUG_MODULE, installModules, installWithDebug, moduleArtifact } from '@digi/regression/runtime/install.ts';
import { expectRuntimeReady, runtimeTests } from '@digi/regression/runtime/ready.ts';
import { ofKind, readSmallRing, RING_SYMBOLS } from '@digi/regression/runtime/runtime-menu.ts';
import { planSymbol, prepareRuntimePlan } from '@digi/regression/runtime/runtime-plan.ts';

const text = (words: number[]) =>
  String.fromCharCode(...words.flatMap((w) => [w >>> 24, (w >>> 16) & 255, (w >>> 8) & 255, w & 255]).filter(Boolean));

/** Without digikit/debug the runtime boots and works, has no log commands, and keeps its small ring of boot and error events. */
async function expectWithoutDebugModule(runtime: PatchedSession): Promise<void> {
  const { session, dk } = runtime;
  await expectRuntimeReady(session);
  const plan = await prepareRuntimePlan(session, liveArtifacts(session, 'debug-module'), RING_SYMBOLS);
  const info = await dk.info();
  expect(hasCap(info, 'SERVICE_CMDS'), 'a runtime that answers').toBe(true);
  for (const cap of ['LOG', 'PEEK', 'LOGCFG'] as const) expect(hasCap(info, cap), `no ${cap} capability`).toBe(false);
  for (const cmd of [SERVICE_CMD.LOG, SERVICE_CMD.PEEK, SERVICE_CMD.LOGCFG])
    expect((await dk.request(cmd, new Uint8Array(6))).status, `command ${cmd.toString(16)} is unknown`).toBe(-60);
  expect(await dk.modules()).toEqual([]);

  const ring = await readSmallRing(session, plan);
  const kinds = ring.map((e) => e.kind);
  expect(kinds, 'the boot events are in the small ring').toEqual(
    expect.arrayContaining([LOG_KIND.BOOT_START, LOG_KIND.UI_FIRST]),
  );
  expect(ring.length, 'the ring holds at most 32 events').toBeLessThanOrEqual(32);
  // only error events and boot events are kept: no periodic HEALTH, DSP_DIAG or MOD_LOG
  for (const kind of [LOG_KIND.HEALTH, LOG_KIND.DSP_DIAG, LOG_KIND.MOD_LOG]) expect(kinds).not.toContain(kind);
  await session.expectNoFault('runtime without the debug module');
}

/** With it: the boot events are there (handed over from the small ring), levels are read and set, PEEK reads RAM inside the device ranges and refuses the rest,
 * and a module's own log line (digikit/probe logs `probe ready` in init) arrives with the module as its source. */
async function expectWithDebugModule(runtime: PatchedSession): Promise<void> {
  const { session, dk } = runtime;
  await expectRuntimeReady(session);
  const plan = await prepareRuntimePlan(session, liveArtifacts(session, 'debug-module'), ['DKRT_STATE']);
  await installWithDebug(runtime, 'digikit/probe');
  const info = await dk.info();
  expect(info.card_read_only, 'the card is ours').toBe(false);
  for (const cap of ['LOG', 'PEEK', 'LOGCFG'] as const) expect(hasCap(info, cap), `${cap} capability`).toBe(true);

  const modules = await dk.modules();
  const probe = modules.find((m) => m.name === 'digikit/probe')!;
  expect(modules.map((m) => m.name)).toEqual(['digikit/debug', 'digikit/probe']);
  await expect
    .poll(async () => (await dk.readLog()).some((e) => e.kind === LOG_KIND.UI_FIRST), {
      timeout: 60_000,
      interval: 1000,
    })
    .toBe(true);
  const log = await dk.readLog();
  const first = (kind: number) => log.find((e) => e.kind === kind);
  expect(first(LOG_KIND.BOOT_START), 'the boot events were handed over').toBeDefined();
  expect(log.filter((e) => e.kind === LOG_KIND.MOD_LOAD)).toHaveLength(2);
  expect(first(LOG_KIND.BOOT_START)!.seq, 'in order').toBeLessThan(first(LOG_KIND.UI_FIRST)!.seq);
  const seqs = log.map((e) => e.seq);
  expect(seqs, 'sequence numbers count up by one').toEqual(seqs.map((_, i) => seqs[0]! + i));

  const modLog = log.find((e) => e.kind === LOG_KIND.MOD_LOG && text(e.args.slice(0, 4)).startsWith('probe ready'));
  expect(modLog, 'the module log line arrives').toBeDefined();
  expect((modLog!.flags >> 8) & 0xff, 'its source is the probe module (record index + 1)').toBe(probe.index + 1);

  // levels: the defaults, then a set that is read back; the levels are in RAM
  expect(await dk.logConfig()).toEqual({ level: 2, modules: Array(8).fill(0xff), debug: true });
  const modulesLevels = Array<number>(8).fill(0xff);
  modulesLevels[probe.index] = 4;
  expect(await dk.setLogConfig(1, modulesLevels)).toEqual({ level: 1, modules: modulesLevels, debug: true });
  expect(await dk.logConfig()).toEqual({ level: 1, modules: modulesLevels, debug: true });
  expect((await dk.request(SERVICE_CMD.LOGCFG, Uint8Array.of(2, 1))).status, 'the old debug switch is gone').toBe(-60);

  // PEEK: the first range of the device file starts at the firmware's data
  const ok = await dk.request(SERVICE_CMD.PEEK, new Uint8Array([...be32(0x40000000), 0, 16]));
  expect(ok.status).toBe(0);
  expect(ok.body.length).toBe(6 + 16);
  expect([...ok.body.subarray(0, 6)]).toEqual([0x40, 0, 0, 0, 0, 16]);
  expect((await dk.request(SERVICE_CMD.PEEK, new Uint8Array([...be32(0x10), 0, 4]))).status, 'outside the ranges').toBe(
    -63,
  );
  expect(
    (await dk.request(SERVICE_CMD.PEEK, new Uint8Array([...be32(0x40000000), 2, 1]))).status,
    'over 512 bytes',
  ).toBe(-63);
  expectLogShape(log, info.runtime);
  await expectPeekRanges(runtime, plan, info.runtime);
  await expectLevelsFilterUploadEvents(runtime);
  await session.expectNoFault('runtime with the debug module');
}

/** The log starts at sequence 1 with the block counter as time (not going back), holds the boot steps, and every event renders as a log line. */
function expectLogShape(log: DkLogEvent[], runtime: number) {
  expect(log[0]!.seq, 'the log starts at sequence 1').toBe(1);
  expect(
    log.every((e, i) => i === 0 || log[i - 1]!.time <= e.time),
    'the block counter does not go back',
  ).toBe(true);
  const kinds = new Set(log.map((e) => e.kind));
  for (const kind of [
    LOG_KIND.BOOT_START,
    LOG_KIND.CARD_CHECK,
    LOG_KIND.RAM_KEEP,
    LOG_KIND.BOOT_RECORD,
    LOG_KIND.UI_FIRST,
  ])
    expect(kinds.has(kind), `boot step ${kind} is logged (handed over from the runtime's ring)`).toBe(true);
  const start = log.find((e) => e.kind === LOG_KIND.BOOT_START)!;
  expect(start.args[0], 'BOOT_START carries the runtime version').toBe(runtime);
  expect([1, 2], 'and the device id').toContain(start.args[1]);
  // Python `dklog follow --once`: every event as a readable line
  const names = moduleNames(log);
  const lines = log.map((e) => eventLine(runtimeEvent(e), names));
  expect(lines.length).toBeGreaterThanOrEqual(8);
  expect(lines.some((line) => line.includes('CARD_CHECK'))).toBe(true);
  expect(
    log.some((e) => e.kind === LOG_KIND.CRASH_FOUND),
    'no crash record in this log',
  ).toBe(false);
}

/** PEEK reads the runtime's own region and the firmware's data range, in 512-byte requests, and refuses everything outside the ranges of the device file. */
async function expectPeekRanges(
  { session, dk }: PatchedSession,
  plan: Awaited<ReturnType<typeof prepareRuntimePlan>>,
  runtime: number,
) {
  const peek = (addr: number, n: number) =>
    dk.request(SERVICE_CMD.PEEK, new Uint8Array([...be32(addr), n >> 8, n & 255]));
  const state = planSymbol(plan, 'DKRT_STATE');
  const head = await peek(state, 8);
  expect([...head.body.subarray(6, 10)], "the runtime's own region: magic DKRT").toEqual([...Buffer.from('DKRT')]);
  expect(Buffer.from(head.body.subarray(10, 14)).readUInt32BE(0), 'and the runtime version').toBe(runtime);
  const big: number[] = [];
  for (let off = 0; off < 1300; off += 512)
    big.push(...(await peek(state + off, Math.min(512, 1300 - off))).body.subarray(6));
  expect(big, 'PEEK of 1300 bytes in three requests (512 at most each)').toHaveLength(1300);
  expect(big.slice(0, 4)).toEqual([...Buffer.from('DKRT')]);

  const ranges = deviceProfile(session.options.device).digikit.peek;
  const [lo0, hi0] = ranges[0]!;
  const hi1 = ranges.at(-1)![1];
  expect((await peek(lo0 + 0x400, 16)).body.length, "the firmware's data range").toBe(6 + 16);
  const refused: Record<string, [number, number]> = {
    'a peripheral register (the reset controller)': [0xec090000, 4],
    'just below the first range': [lo0 - 4, 8],
    'a range that runs past the end of the last': [hi1 - 4, 8],
    'the gap between the ranges': [hi0, 16],
    'address wrap': [0xfffffff0, 32],
    'length 0': [state, 0],
    'length over 512': [state, 513],
  };
  for (const [name, [addr, n]] of Object.entries(refused)) expect((await peek(addr, n)).status, name).toBe(-63);
}

/** LOGCFG default level 0 (error): an info event (ABORT) is dropped, an error event (DELETE of `..`) is kept; level 2 logs the upload command again. */
async function expectLevelsFilterUploadEvents({ dk }: PatchedSession) {
  const uploads = async () => ofKind(await dk.readLog(), LOG_KIND.UPLOAD).length;
  const modules = Array<number>(8).fill(0xff);
  await dk.setLogConfig(0, modules);
  const before = await uploads();
  await dk.request(CMD.ABORT);
  const afterInfo = await uploads();
  await dk.request(CMD.DELETE, Uint8Array.from([0x2e, 0x2e, 0]));
  const afterError = await uploads();
  expect(afterInfo, 'level error: an info event is dropped').toBe(before);
  expect(afterError, 'an error event is kept').toBe(afterInfo + 1);
  expect((await dk.setLogConfig(2, modules)).level).toBe(2);
  await dk.request(CMD.ABORT);
  expect(await uploads(), 'level info: the upload command is logged again').toBe(afterError + 1);
}

/** A coupled boot with the debug module for 32 s: a HEALTH snapshot every ten seconds (15,000 blocks), the stable period (30 s) logged once, DSP diagnostics
 * logged quietly (a full set of eight events at the first poll, then every ten seconds), and a running DSP is never reported stopped. */
async function expectPeriodicEvents(runtime: PatchedSession): Promise<void> {
  const { session, dk } = runtime;
  await expectRuntimeReady(session);
  await installWithDebug(runtime);
  await waitFrames(runtime, 48_000);
  const log = await dk.readLog();
  const health = ofKind(log, LOG_KIND.HEALTH);
  const gap = (a: { time: number }, b: { time: number }) => b.time - a.time;
  expect(health.length, 'a HEALTH snapshot every ten seconds').toBeGreaterThanOrEqual(3);
  const expectTenSeconds = (n: number) => {
    expect(n).toBeGreaterThanOrEqual(14_000);
    expect(n).toBeLessThan(16_500);
  };
  for (let i = 1; i < health.length; i++) expectTenSeconds(gap(health[i - 1]!, health[i]!));
  expect(ofKind(log, LOG_KIND.STABLE), 'the stable period is logged once').toHaveLength(1);

  const diag = ofKind(log, LOG_KIND.DSP_DIAG);
  const starts = diag.filter((e) => (e.args[0]! & 0xff) === 0);
  expect(
    starts.map((e) => e.args[0]! >>> 8),
    'every set holds all 32 words of the DSP diagnostics (watch::DIAG_WORDS)',
  ).toEqual(starts.map(() => 32));
  expect(diag.length, 'a full set (32 words, eight events) at the first poll and at each later one').toBe(
    8 * starts.length,
  );
  expect(starts.length).toBeGreaterThanOrEqual(3);
  expect(starts.length).toBeLessThanOrEqual(5);
  for (let i = 2; i < starts.length; i++) expectTenSeconds(gap(starts[i - 1]!, starts[i]!));
  expect(ofKind(log, LOG_KIND.DSP_STOPPED), 'a running DSP is never reported stopped').toEqual([]);
  await session.expectNoFault('periodic events');
}

/** A module that loads before digikit/debug and logs in its init (digikit/remote: `remote registered`): the small ring kept the line through the boot, and
 * the debug module copies it when it starts. */
async function expectEarlyModuleLog(runtime: PatchedSession): Promise<void> {
  const { session, dk } = runtime;
  await expectRuntimeReady(session);
  const device = session.options.device;
  await installModules(
    dk,
    ['digikit/remote', DEBUG_MODULE].map((id) => ({ id, artifact: moduleArtifact(device, id) })),
  );
  expect((await dk.modules()).map((m) => m.name)).toEqual(['digikit/remote', 'digikit/debug']);
  await expect
    .poll(async () => (await dk.readLog()).some((e) => e.kind === LOG_KIND.UI_FIRST), {
      timeout: 60_000,
      interval: 1000,
    })
    .toBe(true);
  const log = await dk.readLog();
  const line = log.find((e) => e.kind === LOG_KIND.MOD_LOG && text(e.args.slice(0, 4)).startsWith('remote registere'));
  expect(line, 'the log line of the module that loaded before the debug module').toBeDefined();
  expect(log.find((e) => e.kind === LOG_KIND.BOOT_START)!.seq).toBeLessThan(line!.seq);
  await session.expectNoFault('runtime with an early module log line');
}

runtimeTests(
  'without the debug module the runtime boots, has no log commands and keeps a small ring',
  expectWithoutDebugModule,
  {
    timeout: 600_000,
  },
);
runtimeTests('with the debug module the boot events, levels, PEEK and a module log line work', expectWithDebugModule, {
  timeout: 900_000,
});
runtimeTests('a module that loads before the debug module has its log line in the debug log', expectEarlyModuleLog, {
  timeout: 900_000,
});
runtimeTests(
  'the periodic events: HEALTH every ten seconds, STABLE once, quiet DSP diagnostics',
  expectPeriodicEvents,
  {
    timeout: 1_200_000,
    devices: ['dt2'],
    full: true,
  },
);
