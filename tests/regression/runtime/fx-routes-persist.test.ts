import { execFile } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { promisify } from 'node:util';
import { DkSession, EmuUsbTransport } from '@digi/dk-session';
import { button, repoRoot } from '@digi/test-support';
import { describe, expect, test } from 'vitest';
import type { PatchedSession } from '../support.ts';
import { advance, analysisJson, bounded, frames, pickSine, rpc, word } from './fx-live.ts';
import { SEND, SINE, triggerFrame } from './fixtures.ts';
import { withRouteFxSession } from './fx-route-live.ts';
import { inspectFxBoot, withPausedFxProtocol, tap } from './fx-session.ts';
import { MASTER, routeUi, SEND_ROUTE } from './fx-routes-ui.ts';
import { DEVICES, expectRuntimeUi, FIRMWARE, record } from './live.ts';
import { planSymbol } from './runtime-plan.ts';

const exec = promisify(execFile);
const root = repoRoot();
const routeKey = `fx/route/${SEND}`;

interface PcmMetrics {
  identical: boolean;
  rms_base: number;
  rms_db: number | null;
  max_err_ulps: number;
  shift: number | null;
}

async function compareRoutePcm(base: string, sample: string, gain: number, artifacts: string, tag: string) {
  const metrics = await analysisJson<PcmMetrics>(
    'tools.patchkit.hook_ab',
    ['compare', base, sample, '--gain', String(gain), '--align-onset', '--json'],
    artifacts,
    `pcm-${tag}`,
  );
  expect(metrics.rms_base).toBeGreaterThan(0);
  expect(metrics.shift).not.toBeNull();
  // Separate boots may move the onset by up to four 64-sample DSP sub-blocks.
  expect(Math.abs(metrics.shift!)).toBeLessThanOrEqual(256);
  expect(metrics.max_err_ulps).toBeLessThanOrEqual(2);
  expect(metrics.rms_db).not.toBeNull();
  expect(Math.abs(metrics.rms_db! - 20 * Math.log10(gain))).toBeLessThan(0.001);
  if (gain === 1) expect(metrics.identical).toBe(true);
}

/** Native store reader observes the card after guest writes; no settings encoding in this test. */
async function storedRoute(runtime: PatchedSession, artifacts: string, tag: string) {
  const base = runtime.session.cardPath;
  if (!base) throw new Error('route persistence needs the loaded base card');
  const binary = path.join(root, 'out/native/target/nogen/release/patchkit');
  if (!fs.existsSync(binary)) throw new Error(`prepare prebuilt patchkit separately: ${binary}`);
  const overlay = path.join(artifacts, `${tag}.overlay.bin`);
  await rpc(runtime.session, 'card.overlay', { path: overlay });
  const dumped = await exec(
    binary,
    ['store', 'dump', base, '--device', runtime.session.options.device, '--overlay', overlay, '--json'],
    { cwd: root, timeout: 30_000 },
  );
  const dump = JSON.parse(dumped.stdout) as {
    settings: { active: 'A' | 'B' | null; copies: { entries: { key: string; value: string }[] }[] };
  };
  record(artifacts, `${tag}-store`, dump);
  expect(dump.settings.active).not.toBeNull();
  return dump.settings.copies[dump.settings.active === 'A' ? 0 : 1]!.entries.find((entry) => entry.key === routeKey)
    ?.value;
}

for (const device of DEVICES) {
  describe(`${device.toUpperCase()} ${FIRMWARE[device]} FX route persistence`, () => {
    const context = withRouteFxSession(device, 'FX routes dry/apply/back', {
      image: 'routes',
      initial: 'dry',
      presets: {
        dry: { selection: ['modules/m0.dkm'] },
        apply: { selection: ['modules/m0.dkm', 'modules/m2.dkm'] },
        back: { selection: ['modules/m0.dkm', 'modules/m2.dkm'], settings: [[routeKey, '00']] },
      },
      requireAudible: true,
    });

    test('menu YES persists both route directions and restarts into the matching audio and FX pages', async () => {
      const { fixture, plan, artifacts, slots, rememberEpoch } = context;
      const inspect = async (runtime: PatchedSession, tag: string, effect: boolean, afterEpoch?: number) => {
        const state = await inspectFxBoot(runtime, plan, effect ? [SINE, SEND] : [SINE], artifacts, tag, undefined, {
          afterEpoch,
        });
        rememberEpoch(state.epoch);
        return state;
      };
      const capture = async (runtime: PatchedSession, tag: string) => {
        const { session } = runtime;
        await rpc(session, 'run.mode', { mode: 'paused' });
        await rpc(session, 'pcm.tap', { action: 'start' });
        const hint = device === 'dt2' ? 7 : 5;
        const sine = (await withPausedFxProtocol(runtime, (dk) => dk.modules())).find((module) => module.name === SINE);
        expect(sine?.types).toHaveLength(1);
        if (device === 'dt2') {
          let selected = -1;
          for (const position of [hint, hint + 1, hint - 1, hint + 2, hint + 3]) {
            await pickSine(session, position);
            // Legacy DT2 selected-track type observation (finding 02).
            selected = ((await word(session, context.profile.track_type_word)) >>> 8) & 0xff;
            if (selected === sine!.types[0]) break;
          }
          expect(selected, 'the selected DT2 machine is the runtime sine').toBe(sine!.types[0]);
        } else await pickSine(session, hint);
        await tap(session, 'NO', 300);
        const current = await frames(session);
        const trigger = Math.max(triggerFrame(device), current + 300);
        record(artifacts, `${tag}-timing`, { currentFrame: current, triggerFrame: trigger });
        const file = path.join(artifacts, `${tag}.wav`);
        try {
          await bounded(() => session.client.runUntil({ frames: trigger, timeout_wall_s: 30 }));
          await bounded(() => session.client.step(44_000, { timeout_wall_s: 30 }));
          await rpc(session, 'pcm.tap', { action: 'capture', path: file });
          await bounded(() => session.client.inputButton(button('TRIG 1'), true));
          await bounded(() => session.client.runUntil({ frames: trigger + 1500, timeout_wall_s: 30 }));
        } finally {
          try {
            await rpc(session, 'pcm.tap', { action: 'capture_stop' });
          } finally {
            await bounded(() => session.client.inputButton(button('TRIG 1'), false));
          }
        }
        await advance(session, 50);
        await bounded(() => session.expectNoFault(`FX routes ${tag}`));
        return file;
      };
      const expectRoute = async (runtime: PatchedSession, send: boolean) => {
        const ui = routeUi(runtime, plan, artifacts, slots);
        expect(await ui.routes(1)).toEqual([send ? 1 : 0]);
        expect(await ui.countCell()).toBe(send ? 2 : 0);
        expect((await ui.sendUsed())!.used).toBe(send ? 1 : 0);
      };
      const pages = async (runtime: PatchedSession, tag: string, send: boolean) => {
        const ui = routeUi(runtime, plan, artifacts, slots);
        await tap(runtime.session, 'FX', 400);
        const first = await ui.shot(`${tag}-fx1`);
        await tap(runtime.session, 'FX', 400);
        const second = await ui.shot(`${tag}-fx2`);
        if (send) expect(second).not.toBe(first);
      };
      const apply = async (runtime: PatchedSession, tag: 'apply' | 'back', send: boolean) => {
        const { session } = runtime;
        const ui = routeUi(runtime, plan, artifacts, slots);
        await ui.toModules();
        expect((await ui.pending())[0]).toBe(send ? MASTER : SEND_ROUTE);
        expect(await ui.routeChoice(2)).toEqual([SEND_ROUTE, MASTER]);
        await ui.shot(`${tag}-choice`);
        await ui.pick(send ? 0 : 1);
        expect((await ui.pending())[0]).toBe(send ? SEND_ROUTE : MASTER);
        const applies = await ui.applies();
        await tap(session, 'NO', 700);
        await ui.shot(`${tag}-question`);
        expect(await ui.popup()).toEqual(['APPLY & RESTART?', '1 CHANGE']);
        expect(await ui.applies()).toBe(applies);
        await context.evidence(runtime, `${tag}-before-restart`, tag === 'apply');
        const before = (await bounded(() => session.status())).epoch;
        expect(typeof before).toBe('number');
        rememberEpoch(before);
        session.client.drainEvents();
        await tap(session, 'YES', 300);
        expect(await ui.applies()).toBe(applies + 1);
        expect(await word(session, planSymbol(plan, 'DK_FXR_SET'))).toBe(0);
        expect(await word(session, planSymbol(plan, 'DK_FXR_FLUSH'))).toBe(0);
        runtime.dk.cancelPending();
        await rpc(session, 'run.mode', { mode: 'free' });
        const dk = new DkSession(new EmuUsbTransport((method, params) => session.client.call(method, params)));
        const reboot = { session, dk, close: runtime.close };
        try {
          await expectRuntimeUi(session, plan, artifacts, { afterEpoch: Number(before) });
          await bounded(() => session.client.guardSet(context.baseline(tag).guard));
          await inspect(reboot, `${tag}-restarted`, true, Number(before));
          await expectRoute(reboot, send);
          expect(await storedRoute(reboot, artifacts, tag)).toBe(send ? 'send' : 'master');
          const file = await capture(reboot, tag === 'apply' ? 'master' : 'send1');
          await pages(reboot, `${tag}-after`, send);
          await context.evidence(reboot, `${tag}-after-restart`);
          if (!send) {
            const afterUi = routeUi(reboot, plan, artifacts, slots);
            await afterUi.toModules();
            expect((await afterUi.pending())[0]).toBe(MASTER);
            const popups = await afterUi.popups();
            await tap(session, 'NO', 400);
            expect(await afterUi.popups()).toBe(popups);
          }
          return file;
        } finally {
          dk.cancelPending();
        }
      };

      const dryRuntime = fixture.runtime;
      await inspect(dryRuntime, 'dry-boot', false);
      const dry = await capture(dryRuntime, 'dry');
      await context.evidence(dryRuntime, 'dry');
      const sendRuntime = await fixture.reload(context.baseline('apply'));
      await inspect(sendRuntime, 'apply-boot', true);
      await expectRoute(sendRuntime, true);
      const send0 = await capture(sendRuntime, 'send0');
      await pages(sendRuntime, 'apply-before', true);
      const master = await apply(sendRuntime, 'apply', false);
      // Independent clean baseline: two quick retained restarts would trigger unstable-boot safe mode.
      const masterRuntime = await fixture.reload(context.baseline('back'));
      await inspect(masterRuntime, 'back-boot', true);
      await expectRoute(masterRuntime, false);
      const send1 = await apply(masterRuntime, 'back', true);
      await compareRoutePcm(dry, send0, 1, artifacts, 'send0');
      await compareRoutePcm(dry, master, 0.5, artifacts, 'master');
      await compareRoutePcm(dry, send1, 1, artifacts, 'send1');
    }, 300_000);
  });
}
