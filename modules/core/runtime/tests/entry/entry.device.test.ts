import { createHash } from 'node:crypto';
import fs from 'node:fs';
import { RANGES } from '@digi/dk-session/gen_proto';
import { expect } from 'vitest';
import type { PatchedSession } from '@digi/regression/support.ts';
import { liveArtifacts } from '@digi/regression/runtime/artifacts.ts';
import { bootInfo } from '@digi/regression/runtime/card-kit.ts';
import { installModule, moduleArtifact } from '@digi/regression/runtime/install.ts';
import { prepareRuntimePlan } from '@digi/regression/runtime/runtime-plan.ts';
import { E_SET_NOKEY } from '@digi/regression/runtime/project-kit.ts';
import { expectRuntimeReady, runtimeTests } from '@digi/regression/runtime/ready.ts';
import { linkedRuntimeNeed, runtimeVersion } from '@digi/regression/runtime/upload-kit.ts';
import { emulatorClock, waitClock } from '@digi/test-support';
import { pollEmulated, waitForBootEmulated, expectPollEmulated } from '@digi/regression/runtime/waits.ts';

/**
 * The entry point `dk_module` and the service imports (docs/plans/sdk.md section 5; the fixture test/entry-demo uses only the new contract).
 * `init` gets a `dk_module_ctx` and opens the settings; the optional USB bulk service is not in the image, so its `_open` answers DK_E_MISSING and the module
 * still loads; `tick` runs once per UI pass; a settings write queued by a host command runs in `tick`.
 */
const REPORT = 0x1c;
const WRITE = 0x1d;
const E_MISSING = -73;
const E_CONTRACT = -48;
const DEVICE_ID = { dt2: 1, dn2: 2 } as const; // [digikit] id of devices/*.toml

async function expectEntryPoint({ session, dk }: PatchedSession): Promise<void> {
  expect(['zero', 'none'], 'the entry point needs no DSP').toContain((await session.status()).dsp);
  await expectRuntimeReady(session);
  await installModule(dk, 'test/entry-demo', moduleArtifact(session.options.device, 'test/entry-demo'), {
    device: session.options.device,
  });
  expect(RANGES.entry_demo_test, 'reserved command range of the fixture').toEqual([0x1c, 0x1d]);
  const report = async () => {
    const reply = await dk.request(REPORT);
    expect(reply.status, 'the module registered its commands in init').toBe(0);
    const b = Buffer.from(reply.body);
    expect(b.length, 'report length').toBe(42);
    return {
      settingsOpen: b.readInt32BE(0),
      bootRead: b.readInt32BE(4),
      usbOpen: b.readInt32BE(8),
      usbHandleNull: b.readUInt32BE(12),
      device: b.readUInt32BE(16),
      runtime: b.readUInt32BE(20),
      ticks: b.readUInt32BE(24),
      tickSpan: b.readUInt32BE(28),
      put: b.readInt32BE(32),
      back: b.readInt32BE(36),
      value: b[40]!,
      pending: b[41]!,
    };
  };

  const first = await report();
  expect(first.settingsOpen, 'dk_settings_open in init').toBe(0);
  expect(first.bootRead, 'a fresh card has no "boot" value').toBe(E_SET_NOKEY);
  expect(first.usbOpen, 'dk_usb_bulk_open without core/usb-bulk is DK_E_MISSING').toBe(E_MISSING);
  expect(first.usbHandleNull, 'the absent service leaves the handle NULL').toBe(1);
  expect(first.device, 'dk_module_ctx_device').toBe(DEVICE_ID[session.options.device]);
  expect(first.runtime, 'dk_module_ctx_runtime_version').toBe(runtimeVersion());

  // The UI loop (and with it `tick`) starts some emulated seconds after the boot finished and the module registered: wait for the first tick (firmware
  // state), then let a second of emulated time pass.
  const running = await pollEmulated(session, report, (r) => r.ticks > 0, {
    emulatedMs: 30_000,
    message: 'the UI loop never called tick',
  });
  await waitClock(emulatorClock(session), 1000); // a second of ticks: no observable condition beyond the first tick
  const later = await report();
  expect(later.ticks, 'tick runs on every UI pass').toBeGreaterThan(running.ticks);
  expect(later.tickSpan, 'dk_time_blocks advances between the first and the last tick').toBeGreaterThan(0);

  expect((await dk.request(WRITE, Uint8Array.of(0x5a))).status, 'the write is queued').toBe(0);
  await waitClock(emulatorClock(session), 500); // the tick that runs the queued write has no event
  const written = await report();
  expect(written.pending, 'tick ran the queued write').toBe(0);
  expect([written.put, written.back, written.value], 'dk_settings_put, then dk_settings_get in tick').toEqual([0, 1, 0x5a]);
  await session.expectNoFault('entry point');
}

runtimeTests('a module of the new entry point loads, ticks and finds an optional service absent', expectEntryPoint, {
  dsp: 'zero',
});

/**
 * Version refusal. The packer writes the runtime version a module needs (the newest `since` among the imports it links, in sdk/sdk.toml) into the header word at 0x30
 * (the imports of test/entry-demo). Copies that need another major, another minor or a higher patch than the runtime's, each with the sha256 made right again so that
 * only the version is wrong, are refused with E_CONTRACT and the module list shows no second entry; the file with the real version loads after them.
 */
async function expectVersionRefusal({ session, dk }: PatchedSession): Promise<void> {
  const device = session.options.device;
  await expectRuntimeReady(session);
  const plan = await prepareRuntimePlan(session, liveArtifacts(session, 'entry-version'), ['DKRT_BOOT']);
  const good = fs.readFileSync(moduleArtifact(device, 'test/entry-demo'));
  const need = linkedRuntimeNeed(good);
  expect(need, 'the newest `since` among the linked imports is the header word').toBe(good.readUInt32BE(0x30) & 0x7fffffff);
  expect(need >> 8, 'the module needs this runtime\'s major and minor').toBe(runtimeVersion() >> 8);
  const withNeed = (version: number): Buffer => {
    const file = Buffer.from(good);
    file.writeUInt32BE(version, 0x30);
    file.fill(0, 0x10, 0x30);
    createHash('sha256').update(file).digest().copy(file, 0x10);
    return file;
  };
  const runtime = runtimeVersion();
  await dk.put('modules/entry-major.dkm', withNeed(runtime + 0x10000));
  await dk.put('modules/entry-minor.dkm', withNeed(runtime + 0x100));
  await dk.put('modules/entry-patch.dkm', withNeed(runtime | 0xff));
  await dk.put('modules/entry-demo.dkm', good);
  await dk.select(['modules/entry-major.dkm', 'modules/entry-minor.dkm', 'modules/entry-patch.dkm', 'modules/entry-demo.dkm']);
  await dk.restart();
  await waitForBootEmulated(dk);
  await expectPollEmulated(session, async () => (await dk.modules()).map((m) => m.name), (value) => expect(value).toEqual(['test/entry-demo']), { emulatedMs: 30000, intervalMs: 500 });
  const boot = await bootInfo(session, plan);
  expect(boot.results, 'another major, another minor or a higher patch is E_CONTRACT, the real one loads').toEqual([E_CONTRACT, E_CONTRACT, E_CONTRACT, 0]);
  await session.expectNoFault('version refusal');
}

runtimeTests('a module that needs another runtime major or minor, or a higher patch, than the runtime is refused with E_CONTRACT', expectVersionRefusal, {
  dsp: 'zero',
});
