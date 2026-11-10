import type { Device, Session, SessionOptions } from '@digi/test-support';
import { describe, expect, test } from 'vitest';
import { expectRuntimeBoot, withPatchedSession, type PatchedSession } from '../support.ts';
import { timePhase, timingContext } from '../timing.ts';
import { liveArtifacts, record } from './artifacts.ts';
import { FIRMWARE, FULL_TIER, liveOptions, selectDevices, skipEmptyHalf } from './devices.ts';
import { planSymbol, prepareRuntimePlan, type RuntimePlan } from './runtime-plan.ts';
import { beforeReadinessDeadline } from './waits.ts';

/** One test per device, each on its own fresh runtime daemon that has passed `expectRuntimeBoot`. */
export function runtimeTests(
  name: string,
  body: (runtime: PatchedSession) => Promise<void>,
  opts: {
    patchSet?: string;
    dsp?: SessionOptions['dsp'];
    mirror?: boolean;
    timeout?: number;
    devices?: readonly Device[];
    full?: boolean;
    skip?: string;
  } = {},
): void {
  const selected = selectDevices(opts.devices);
  skipEmptyHalf(selected, name);
  for (const device of selected) {
    describe.skipIf((opts.full === true && !FULL_TIER) || opts.skip !== undefined)(
      `${device.toUpperCase()} ${FIRMWARE[device]}`,
      () => {
        const handle = withPatchedSession({ ...liveOptions(device, opts), dsp: opts.dsp }, name);
        test(
          name,
          async () => {
            await expectRuntimeBoot(handle.runtime);
            await timePhase('scenario.body', () => body(handle.runtime), timingContext(handle.runtime.session));
          },
          opts.timeout,
        );
      },
    );
  }
}

/** Wait for the runtime's normal UI hook on a fresh free-running daemon, before the first install. */
export async function expectRuntimeReady(session: Session): Promise<void> {
  const artifacts = liveArtifacts(session, 'runtime-ready');
  const plan = await prepareRuntimePlan(session, artifacts, ['DKRT_BOOT']);
  await timePhase('ready', () => expectRuntimeUi(session, plan, artifacts), timingContext(session));
}

/** DkrtBootInfo word offsets, owned by modules/core/runtime/abi.toml (checked in modules/core/runtime/sdk/src/gen_abi.rs). */
export const BOOT_FIELDS = {
  magic: 0,
  version: 4,
  safe: 8,
  reason: 12,
  counter: 16,
  count: 28,
  ui_reached: 36,
} as const;

/** Current UI-hook evidence, rather than elapsed time or the boot's in-memory stability flag. */
export async function expectRuntimeUi(
  session: Session,
  plan: RuntimePlan,
  artifacts: string,
  options: { fresh?: boolean; afterEpoch?: number; timeout?: number; paused?: boolean } = {},
): Promise<number> {
  // Wall time is only a hang guard: readiness is the firmware state polled below. A DN2 reboot takes 45-75 s of wall time
  // under load, so the guard matches the lifecycle deadline of reloadRuntimeBaseline (120 s).
  const timeout = options.timeout ?? 120_000;
  const deadline = Date.now() + timeout;
  const base = planSymbol(plan, 'DKRT_BOOT');
  let status: Awaited<ReturnType<Session['status']>> | undefined;
  let boot: Record<keyof typeof BOOT_FIELDS, number | null> | undefined;
  try {
    await expect
      .poll(
        async () => {
          status = await beforeReadinessDeadline(deadline, () => session.status());
          boot = Object.fromEntries(
            await beforeReadinessDeadline(deadline, () =>
              Promise.all(
                Object.entries(BOOT_FIELDS).map(async ([name, offset]) => [
                  name,
                  await session.client.memPeek(base + offset),
                ]),
              ),
            ),
          ) as typeof boot;
          return (
            status.mode === (options.paused ? 'paused' : 'free') &&
            // Restart preserves the card and relaxes the DT2 filesystem-worker check.
            // A new epoch still needs the current UI guards; an explicit filesystem failure is never accepted.
            (options.afterEpoch === undefined ? status.cf.ready : status.cf.filesystem_verified !== false) &&
            status.cf.main_ui_reached &&
            typeof status.epoch === 'number' &&
            (options.afterEpoch === undefined || status.epoch > options.afterEpoch) &&
            boot?.magic === 0x444b4254 &&
            boot.version === 1 &&
            boot.safe === 0 &&
            boot.reason === 0 &&
            boot.ui_reached === 1
          );
        },
        {
          timeout,
          interval: 500,
          message: 'current runtime boot reached the normal UI hook',
        },
      )
      .toBe(true);
    if (options.fresh) {
      // boot.rs only raises the persisted counter when count != 0 or counter != 0.
      // This fresh empty boot and one controlled install restart need no stability wait.
      expect(boot?.count, 'fresh pre-install boot has no selected modules').toBe(0);
      expect(boot?.counter, 'fresh pre-install boot found no failed boots').toBe(0);
    }
    if (typeof status?.epoch !== 'number') throw new Error('runtime status has no boot epoch');
    return status.epoch;
  } finally {
    record(artifacts, options.fresh ? 'readiness-pre-install' : 'readiness-post-install', {
      afterEpoch: options.afterEpoch,
      status: status ?? null,
      boot: boot ?? null,
      expired: Date.now() >= deadline,
    });
  }
}
