import fs from 'node:fs';
import { decodeFault, readSettingsRecord, settingsValue } from '@digi/dk-session';
import { CAP, CMD, LOG_KIND, SERVICE_CMD } from '@digi/dk-session/gen_proto';
import { button, MS } from '@digi/test-support';
import { expect } from 'vitest';
import type { PatchedSession } from '@digi/regression/support.ts';
import { liveArtifacts } from '@digi/regression/runtime/artifacts.ts';
import { DEBUG_MODULE, installWithDebug, moduleArtifact } from '@digi/regression/runtime/install.ts';
import { BOOT_FIELDS, expectRuntimeReady, runtimeTests } from '@digi/regression/runtime/ready.ts';
import {
  openDigikitMenu,
  readAboutRows,
  readCrashSeen,
  readSmallRing,
  RING_SYMBOLS,
} from '@digi/regression/runtime/runtime-menu.ts';
import { planSymbol, prepareRuntimePlan } from '@digi/regression/runtime/runtime-plan.ts';

const SOURCE_SHIFT = 8;
/** The watch declares a stop after 4500 blocks (3 s) without a heartbeat; the daemon emulates the DSP far slower than 1500 blocks a second. */
const BLOCKS_PAST_THE_LIMIT = 9000;
/** The ABOUT walk holds each key this many emulated ms, timed by the daemon in ColdFire instructions like the stock tests. */
const TAP_HOLD_MS = 100;

/** Wait until the DSP has run `blocks` more frames (emulated time, not wall time). */
async function waitFrames({ session }: PatchedSession, blocks: number): Promise<void> {
  const frames = async () => Number(((await session.status()).audio as { frames?: number } | null)?.frames ?? 0);
  const start = await frames();
  await expect
    .poll(async () => (await frames()) - start, { timeout: 900_000, interval: 1000, message: `${blocks} DSP blocks` })
    .toBeGreaterThanOrEqual(blocks);
}
const SITE_VOICE = 1;
const SITE_FX_SEND = 3;
const STAGE_CALL = 4;
const SAFE_DSP = 4;
const SAFE_PANIC = 5;

/** Press, hold and release a key, then let the UI settle, all in DSP blocks (emulated time): the daemon may run far slower than real time, and a wall-clock
 * settle would find the UI not yet done. `settleMs` is the milliseconds the other runtime tests wait; a block is 2/3 ms, and a third of the time suffices here. */
function panelTap(runtime: PatchedSession) {
  const { session } = runtime;
  return async (name: string, settleMs = 1200) => {
    const code = button(name);
    await session.client.inputButton(code, true);
    await waitFrames(runtime, 150);
    await session.client.inputButton(code, false);
    await waitFrames(runtime, Math.round(settleMs / 2));
  };
}

/** The rows of the ABOUT page, opened through the panel like a user does. */
async function aboutRows(runtime: PatchedSession, plan: Awaited<ReturnType<typeof prepareRuntimePlan>>) {
  // keys held in emulated time by the daemon: wall-clock holds lost keys under gate load, and holds that end on a frame poll
  // last up to a second, which the Digitakt II SETTINGS key ignored after the safe boot
  const tap = async (name: string, settleMs = 1200) => {
    await runtime.session.client.inputTap(button(name), TAP_HOLD_MS * MS);
    await waitFrames(runtime, Math.round(settleMs * 1.5));
  };
  await openDigikitMenu(tap);
  // in safe mode opening DIGIKIT shows a dialog that names the reason (NO closes it), and the first row is the SAFE notice: ABOUT is the second
  await tap('NO', 1000);
  // the cursor may rest below the first row after the dialog: go to the top, then to ABOUT
  await tap('UP', 600);
  await tap('UP', 600);
  await tap('UP', 600);
  await tap('DOWN');
  await tap('YES', 3000);
  return readAboutRows(runtime.session, plan);
}

/** Wait for the device to restart itself into safe mode (the hang record is written about 20 s after the DSP stopped). */
async function waitForSafeMode({ dk }: PatchedSession): Promise<void> {
  await expect
    .poll(async () => (await dk.info(true)).safe_mode, {
      timeout: 900_000,
      interval: 2000,
      message: 'safe mode after the stop',
    })
    .toBe(true);
}

/** Wait until the safe boot has reached the UI loop: DKRT_BOOT has `safe` set and `ui_reached` raised (the first UI pass). The main OS starts the UI
 * loop after the boot intro, and the boot hook has already kept `dk/fault` and the log events by then. Wall time is only a hang guard. */
async function waitForSafeUi({ session }: PatchedSession, plan: Awaited<ReturnType<typeof prepareRuntimePlan>>): Promise<void> {
  const base = planSymbol(plan, 'DKRT_BOOT');
  await expect
    .poll(
      async () => {
        const [safe, ui] = await Promise.all([
          session.client.memPeek(base + BOOT_FIELDS.safe),
          session.client.memPeek(base + BOOT_FIELDS.ui_reached),
        ]);
        return safe === 1 && ui === 1;
      },
      { timeout: 300_000, interval: 1000, message: 'the safe boot reached the UI loop' },
    )
    .toBe(true);
}

/** After a stop or a crash the boot is safe mode: INFO has no LOG capability (the debug module is not loaded). Returns the runtime's own ring. */
async function expectNoDebugModule(runtime: PatchedSession, plan: Awaited<ReturnType<typeof prepareRuntimePlan>>) {
  const { session, dk } = runtime;
  const info = await dk.info(true);
  expect(info.safe_mode).toBe(true);
  expect(info.caps !== null && (info.caps >>> CAP.LOG) & 1, 'no LOG capability without the debug module').toBe(0);
  expect((await dk.request(SERVICE_CMD.LOG, Uint8Array.of(0, 0, 0, 0), true)).status).toBe(-60);
  return readSmallRing(session, plan);
}

/** An effect on the send route stops the DSP: the next boot is safe mode, and ABOUT, dk/fault and the log name the module, the site and the step. */
async function expectFxHang(runtime: PatchedSession): Promise<void> {
  const { session, dk } = runtime;
  await expectRuntimeReady(session);
  const plan = await prepareRuntimePlan(session, liveArtifacts(session, 'fault-trace'), [
    'DK_ABOUT',
    'DK_ABOUT_N',
    'DKRT_BOOT',
    ...RING_SYMBOLS,
  ]);
  await installWithDebug(runtime, 'test/dyn-fx-hang');
  await waitForSafeMode(runtime);
  await waitForSafeUi(runtime, plan);

  // the card keeps the fault
  const fault = decodeFault(settingsValue(await readSettingsRecord(dk), 'dk/fault') ?? new Uint8Array());
  expect(fault, 'dk/fault is in the settings record').not.toBeNull();
  expect(fault).toMatchObject({
    kind: 'dsp',
    site: SITE_FX_SEND,
    stage: STAGE_CALL,
    step: 2, // the step of the last block that ended: the step 3 of the call that stopped the DSP is lost with the core
    module: 'dyn-fx-hang',
  });

  // safe mode loads no module, so the debug module is not there to answer LOG: the log of the safe boot is the runtime's own ring, which has the
  // finding and the reason, and the record's tail (taken from that ring at the stop) was found
  const log = await expectNoDebugModule(runtime, plan);
  const found = log.filter((e) => e.kind === LOG_KIND.FAULT_FOUND);
  expect(
    found.map((e) => e.args[0]! & 0xff),
    'FAULT_FOUND of kind DSP stopped',
  ).toEqual([2]);
  expect(found[0]!.args[2], 'the module step in the event').toBe(2);
  expect(log.filter((e) => e.kind === LOG_KIND.SAFE).map((e) => e.args[0])).toEqual([SAFE_DSP]);
  expect((await readCrashSeen(session, plan)).tail, 'the last events of the boot before').toBeGreaterThan(0);
  expect(
    (await dk.modules()).map((m) => m.name),
    'safe mode loads no module',
  ).toEqual([]);

  const rows = await aboutRows(runtime, plan);
  expect(rows).toEqual(
    expect.arrayContaining(['SAFE: DSP STOPPED', 'FAULT: DSP STOP', 'IN DYN-FX-HANG', 'FX SEND STEP 2']),
  );
  for (const row of rows) expect(row.length, row).toBeLessThanOrEqual(22);
  await session.expectNoFault('fx hang');
}

/** A machine whose render never returns: the DT2 voice site. The machine is put on track 1 through the SRC list (the first run-time machine is the 8th row). */
async function expectVoiceHang(runtime: PatchedSession): Promise<void> {
  const { session, dk } = runtime;
  await expectRuntimeReady(session);
  const plan = await prepareRuntimePlan(session, liveArtifacts(session, 'fault-trace'), ['DKRT_BOOT']);
  await installWithDebug(runtime, 'test/dyn-dsp-hang');
  const tap = panelTap(runtime);
  await session.client.inputButton(button('FUNC'), true);
  await tap('SRC', 1200);
  await session.client.inputButton(button('FUNC'), false);
  for (let i = 0; i < 12; i++) await tap('UP', 300);
  for (let i = 0; i < 7; i++) await tap('DOWN', 300);
  await tap('YES', 1200);
  await tap('YES', 1200);
  await expect
    .poll(
      async () =>
        (await dk.readLog(true)).some(
          (e) =>
            e.kind === LOG_KIND.MACHINE_CHANGE && e.args[0] === 0 && (e.args[2] ?? 0) >= 7 && e.args[2] !== 0xffffffff,
        ),
      { timeout: 300_000, interval: 2000, message: 'the run-time machine is on track 1' },
    )
    .toBe(true);
  await session.client.inputButton(button('TRIG 1'), true); // the render hangs on the third block of the note
  await waitForSafeMode(runtime);
  await waitForSafeUi(runtime, plan);

  const fault = decodeFault(settingsValue(await readSettingsRecord(dk), 'dk/fault') ?? new Uint8Array());
  expect(fault).toMatchObject({ kind: 'dsp', site: SITE_VOICE, stage: STAGE_CALL, step: 0, module: 'dyn-dsp-hang' });
  expect(fault!.id, 'a run-time machine type').toBeGreaterThanOrEqual(7);
  await session.expectNoFault('voice hang');
}

/** A send effect that faults in every block: FX_FAULT and FX_DISABLED, tagged with the module's source, and no stop. */
async function expectFxFaultEvents(runtime: PatchedSession): Promise<void> {
  const { session, dk } = runtime;
  await expectRuntimeReady(session);
  await installWithDebug(runtime, 'test/dyn-fx-nan');
  await expect
    .poll(async () => (await dk.readLog(true)).some((e) => e.kind === LOG_KIND.FX_DISABLED), {
      timeout: 600_000,
      interval: 2000,
      message: 'the guard switched the row off',
    })
    .toBe(true);
  const log = await dk.readLog();
  const faults = log.filter((e) => e.kind === LOG_KIND.FX_FAULT);
  const disabled = log.filter((e) => e.kind === LOG_KIND.FX_DISABLED);
  expect(faults.length).toBeGreaterThanOrEqual(1);
  expect(disabled).toHaveLength(1);
  for (const e of [...faults, ...disabled]) {
    expect(e.flags & 1, 'an error event').toBe(1);
    expect((e.flags >> SOURCE_SHIFT) & 0xff, 'the source is the module that owns the row').toBeGreaterThan(0);
    expect(e.args[0], 'the first free effect row').toBe(0);
  }
  expect(faults[faults.length - 1]!.args[1], 'faults counted').toBeGreaterThanOrEqual(8);
  const names = (await dk.modules()).map((m) => m.name);
  expect(names).toContain('test/dyn-fx-nan');
  expect((await dk.info()).safe_mode, 'a faulting effect is no stop').toBe(false);
  expect(log.some((e) => e.kind === LOG_KIND.DSP_STOPPED && (e.flags & 1) !== 0)).toBe(false);
  await session.expectNoFault('fx fault events');
}

/** An ordinary effect runs for longer than the stall limit: no stop is declared and the device does not reset. */
async function expectNoFalseStop(runtime: PatchedSession): Promise<void> {
  const { session, dk } = runtime;
  await expectRuntimeReady(session);
  await installWithDebug(runtime, 'test/dyn-fx-gain-send');
  const boot = (await dk.readLog()).find((e) => e.kind === LOG_KIND.BOOT_START);
  await waitFrames(runtime, BLOCKS_PAST_THE_LIMIT);
  const log = await dk.readLog();
  expect(log.find((e) => e.kind === LOG_KIND.BOOT_START)?.seq, 'the same boot is still running').toBe(boot?.seq);
  expect(log.filter((e) => e.kind === LOG_KIND.DSP_STOPPED)).toEqual([]);
  expect((await dk.info()).safe_mode).toBe(false);
  expect((await dk.modules()).map((m) => m.name)).toContain('test/dyn-fx-gain-send');
  await session.expectNoFault('no false stop');
}

/** A module that raises a CPU exception: the next boot keeps dk/fault with kind CPU crash and the PC. */
async function expectCpuCrashFault(runtime: PatchedSession): Promise<void> {
  const { session, dk } = runtime;
  await expectRuntimeReady(session);
  // the daemon stops the run at a CPU exception unless the guest's handler (the runtime's crash capture) is to run
  await session.client.call('cpu.exceptions', { deliver: true });
  const cfg = Uint8Array.of(0, 0, 0x0b, 0xb8, 0); // 3000 blocks (2 s) after the first slot call, an illegal instruction
  await dk.put('cfg/crash-after.bin', cfg);
  const device = session.options.device;
  const plan = await prepareRuntimePlan(session, liveArtifacts(session, 'fault-trace'), ['DK_ABOUT', 'DKRT_BOOT', ...RING_SYMBOLS]);
  for (const id of [DEBUG_MODULE, 'test/dyn-crash']) {
    await dk.put(`modules/${id.split('/')[1]}.dkm`, fs.readFileSync(moduleArtifact(device, id)));
  }
  await dk.select(['modules/debug.dkm', 'modules/dyn-crash.dkm']);
  await dk.restart();
  // the module crashes this boot and the next one; the third boot is safe mode (two bad boots)
  await waitForSafeMode(runtime);
  await waitForSafeUi(runtime, plan);
  const fault = decodeFault(settingsValue(await readSettingsRecord(dk), 'dk/fault') ?? new Uint8Array());
  expect(fault).toMatchObject({ kind: 'cpu', module: 'dyn-crash' });
  // the safe boot has no debug module: the runtime's own ring has the crash event, and the record's tail came from that ring
  const log = await expectNoDebugModule(runtime, plan);
  const crash = log.find((e) => e.kind === LOG_KIND.CRASH_FOUND);
  expect(fault!.at, 'the PC of the crash record').toBe(crash!.args[0]);
  expect((await readCrashSeen(session, plan)).tail, 'the last events before the crash').toBeGreaterThan(0);
}

/** A panic of the runtime (CMD_FAULT, a failed bounds check) writes a panic record and resets: the next boot is safe mode and ABOUT, dk/fault and the log name it. */
async function expectRuntimePanic(runtime: PatchedSession): Promise<void> {
  const { session, dk } = runtime;
  await expectRuntimeReady(session);
  const plan = await prepareRuntimePlan(session, liveArtifacts(session, 'fault-trace'), [
    'DK_ABOUT',
    'DK_ABOUT_N',
    'DKRT_BOOT',
    ...RING_SYMBOLS,
  ]);
  // the device resets inside the handler and never replies; one try (quick), since a retry would reach the safe boot and panic it too
  await dk.request(CMD.FAULT, Uint8Array.of(1), true).catch(() => undefined);
  await waitForSafeMode(runtime);
  await waitForSafeUi(runtime, plan);

  const fault = decodeFault(settingsValue(await readSettingsRecord(dk), 'dk/fault') ?? new Uint8Array());
  expect(fault, 'dk/fault is in the settings record').not.toBeNull();
  expect(fault).toMatchObject({ kind: 'panic', module: 'runtime', at: 0 });

  const log = await expectNoDebugModule(runtime, plan);
  const found = log.filter((e) => e.kind === LOG_KIND.FAULT_FOUND);
  expect(
    found.map((e) => e.args[0]! & 0xff),
    'FAULT_FOUND of kind panic',
  ).toEqual([3]);
  expect(log.filter((e) => e.kind === LOG_KIND.SAFE).map((e) => e.args[0])).toEqual([SAFE_PANIC]);
  expect((await readCrashSeen(session, plan)).panic, 'the boot found a panic record').toBe(1);

  const rows = await aboutRows(runtime, plan);
  expect(rows).toEqual(expect.arrayContaining(['SAFE: CPU PANIC', 'FAULT: CPU PANIC', 'IN RUNTIME']));
  for (const row of rows) expect(row.length, row).toBeLessThanOrEqual(22);
  await session.expectNoFault('runtime panic');
}

runtimeTests('an effect that stops the DSP gives safe mode and names the module, site and step', expectFxHang, {
  timeout: 1_200_000,
});
runtimeTests('a machine that stops the DSP gives safe mode and names the module and the voice site', expectVoiceHang, {
  timeout: 1_200_000,
  devices: ['dt2'],
});
runtimeTests('effect faults are logged with the module as source', expectFxFaultEvents, { timeout: 900_000 });
runtimeTests('a normal effect is no stop', expectNoFalseStop, { timeout: 1_200_000 });
runtimeTests('a CPU crash is kept as the last fault', expectCpuCrashFault, { timeout: 1_200_000, devices: ['dt2'] });
runtimeTests('a panic of the runtime gives safe mode and names the runtime', expectRuntimePanic, {
  timeout: 1_200_000,
});
