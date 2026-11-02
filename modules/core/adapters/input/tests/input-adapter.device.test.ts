import fs from 'node:fs';
import { button, type Device } from '@digi/test-support';
import { describe, expect, test } from 'vitest';
import { startPatchedSession } from '@digi/regression/support.ts';
import { timePhase, timingContext } from '@digi/regression/timing.ts';
import { prepareRuntimePlan } from '@digi/regression/runtime/runtime-plan.ts';
import { openDigikitMenu, openRuntimeAbout, meaningfulAboutRows } from '@digi/regression/runtime/runtime-menu.ts';
import {
  DEVICES,
  FIRMWARE,
  liveArtifacts,
  liveOptions,
  liveProtocol,
  moduleArtifact,
  record,
  expectRuntimeUi,
  tap as holdTap,
} from '@digi/regression/runtime/live.ts';

/** Two fresh daemons with identical probe installation and exactly one protocol restart. */
async function expectInputAdapter(device: Device): Promise<void> {
  // Probe DKM preparation is separate; the exact-image symbol plan uses the existing patch CLI below.
  const bytes = fs.readFileSync(moduleArtifact(device, 'test/input-probe'));
  const results: { frames: Record<string, string>; rows: string[] }[] = [];
  for (const source of ['panel', 'probe'] as const) {
    const runtime = await startPatchedSession({ ...liveOptions(device), autoNo: false }, `input adapter ${source}`);
    const { session } = runtime;
    let protocol = liveProtocol(session);
    const artifacts = liveArtifacts(session, 'input-adapter');
    const frames: Record<string, string> = {};
    const answers: number[] = [];
    const statuses: unknown[] = [];
    let rows: string[] = [];
    let refusals: number[] = [];
    try {
      await timePhase(
        'scenario.body',
        async () => {
          const plan = await prepareRuntimePlan(session, artifacts, ['DK_ABOUT', 'DK_ABOUT_N', 'DKRT_BOOT']);
          const epoch = await expectRuntimeUi(session, plan, artifacts, { fresh: true });
          await timePhase('module.upload', () => protocol.put('modules/input-probe.dkm', bytes));
          expect(
            await protocol.verifyFile('modules/input-probe.dkm', bytes),
            'prepared probe bytes read back from the card',
          ).toBe(true);
          await protocol.select(['modules/input-probe.dkm']);
          await timePhase('protocol.restart', () => protocol.restart());
          protocol.cancelPending();
          protocol = liveProtocol(session);
          await timePhase('protocol.waitForBoot', () => protocol.waitForBoot());
          await expect
            .poll(async () => (await protocol.modules()).map((module) => module.name), {
              timeout: 20_000,
              interval: 500,
              message: 'input probe imports core/input',
            })
            .toContain('test/input-probe');
          await expectRuntimeUi(session, plan, artifacts, { afterEpoch: epoch });
          const tap = async (name: string, settleMs = 1200) => {
            const code = button(name);
            await holdTap(
              session,
              async (down) => {
                if (source === 'panel') await session.client.inputButton(code, down);
                else {
                  const status = (await protocol.request(0x38, Uint8Array.of(code, down ? 1 : 0))).status;
                  answers.push(status);
                  expect(status, `probe ${name} ${down ? 'down' : 'up'}`).toBe(0);
                }
              },
              300,
              settleMs,
            );
          };
          // Delivery/decoder acceptance, not a claim that a vendor parameter changes in this UI state.
          for (const delta of [4, -4]) {
            if (source === 'panel') {
              const before = await session.status();
              statuses.push(before);
              expect(typeof before.cf.input_irqs).toBe('number');
              await session.client.inputTurn(1, delta);
              await expect
                .poll(
                  async () => {
                    const after = await session.status();
                    statuses.push(after);
                    return after.cf.input_pending === 0 && Number(after.cf.input_irqs) > Number(before.cf.input_irqs);
                  },
                  { timeout: 8000, interval: 400, message: `panel TURN ${delta} delivered through UART` },
                )
                .toBe(true);
            } else {
              const status = (await protocol.request(0x39, Uint8Array.of(1, delta & 255))).status;
              answers.push(status);
              expect(status, `probe TURN ${delta} accepted by input_turn`).toBe(0);
            }
          }
          await openDigikitMenu(tap, async (name) => {
            const frame = await session.client.framebufferGet({ hash: true, data: true });
            expect(frame.sha256).toMatch(/^[a-f0-9]{64}$/);
            frames[name] = frame.sha256!;
            record(artifacts, `${source}-${name}`, frame);
          });
          rows = await openRuntimeAbout(tap, session, protocol, plan, artifacts);
          if (source === 'probe') {
            refusals = [
              (await protocol.request(0x38, Uint8Array.of(200, 1))).status,
              (await protocol.request(0x39, Uint8Array.of(99, 1))).status,
            ];
            expect(refusals, 'invalid key and encoder are refused').toEqual([-1, -1]);
            expect(frames, 'known SETTINGS/DIGIKIT menu frames match the panel').toEqual(results[0]!.frames);
            expect(meaningfulAboutRows(rows), 'ABOUT identity and boot rows match the panel').toEqual(results[0]!.rows);
          }
          await session.expectNoFault(`input adapter ${source}`);
          results.push({ frames, rows: meaningfulAboutRows(rows) });
        },
        timingContext(session),
      );
    } finally {
      try {
        record(artifacts, source, { frames, rows, answers, refusals, statuses });
      } finally {
        protocol.cancelPending();
        await runtime.close();
      }
    }
  }
}

for (const device of DEVICES) {
  describe(`${device.toUpperCase()} ${FIRMWARE[device]}`, () => {
    test(
      'input adapter matches panel menu navigation and accepts signed TURN after identical boots',
      () => expectInputAdapter(device),
      1_000_000,
    );
  });
}
