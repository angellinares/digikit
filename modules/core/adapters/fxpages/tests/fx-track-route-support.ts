import { execFile } from 'node:child_process';
import path from 'node:path';
import { promisify } from 'node:util';
import { repoRoot } from '@digi/test-support';
import { describe, expect, test } from 'vitest';
import type { PatchedSession } from '@digi/regression/support.ts';
import { rpc, rows } from '@digi/regression/runtime/fx-live.ts';
import { FX, SINE, triggerFrame as triggerFor } from '@digi/regression/runtime/fixtures.ts';
import { withRouteFxSession } from '@digi/regression/runtime/fx-route-live.ts';
import { inspectFxBoot, withPausedFxProtocol, selectSine, shot, tap } from '@digi/regression/runtime/fx-session.ts';
import { trackNote } from '@digi/regression/runtime/fx-track-ui.ts';
import { record } from '@digi/regression/runtime/artifacts.ts';
import { patchkitBinary } from '@digi/regression/runtime/patchkit.ts';
import { DEVICES, FIRMWARE } from '@digi/regression/runtime/devices.ts';
import { planSymbol } from '@digi/regression/runtime/runtime-plan.ts';
import type { Device } from '@digi/test-support';

// Shared setup of the track FX send page device tests (fx-track-route-*.device.test.ts). Every file runs its legs from an overlay-free card
// and ready snapshot and boots on its own; the image without core/fxpages closes before the regular image starts, and only artifacts cross that
// boundary. A file that compares pixels or PCM with the stock image (`base: true`) captures them first in this helper's base test.

const exec = promisify(execFile);
export const names = [FX, `${FX}-2`, `${FX}-3`];
export const routeKey = (name: string) => `fx/route/${name}`;
export const sendKey = `fx/send/${FX}`;
const sineCard = { selection: ['modules/m0.dkm'] };
const masterCard = { selection: ['modules/m0.dkm', 'modules/m1.dkm'] };
const sendCard = { ...masterCard, settings: [[routeKey(FX), '01']] as [string, string][] };
const limitCard = {
  selection: ['modules/m0.dkm', 'modules/m1.dkm', 'modules/m2.dkm', 'modules/m3.dkm'],
  settings: names.map((name): [string, string] => [routeKey(name), '01']),
};
const CARDS = { sine: sineCard, master: masterCard, send: sendCard, limit: limitCard };
type Preset = keyof typeof CARDS;

interface StoreDump {
  settings: {
    active: string | null;
    copies: { copy: string; seq: number; entries: { key: string; value_hex: string }[] }[];
  };
}

export type TrackRoute = ReturnType<typeof makeTrackRoute>;

function makeTrackRoute(
  device: Device,
  context: ReturnType<typeof withRouteFxSession>,
  triggerFrame: number,
  base: { hash: string; pcm: string },
) {
      const pcm = new Map<string, string>();
      const locks = new Map<string, { overlay: string; cc: string; shown: number }>();
      const sym = (name: string) => planSymbol(context.plan, name);
      const chain = (runtime: PatchedSession) => rows(runtime.session, context.plan, context.slots);
      const amounts = async (runtime: PatchedSession, row = 0) => {
        const words = (await chain(runtime))[row]!.slice(19, 23);
        return Array.from({ length: 16 }, (_, track) => (words[track >> 2]! >>> (8 * (track % 4))) & 0xff);
      };
      const capacity = async (runtime: PatchedSession, used: number) => {
        expect((await withPausedFxProtocol(runtime, (dk) => dk.capacity())).SEND_SLOTS).toEqual({
          limit: context.sendSlots,
          used,
          largest: 0,
        });
      };
      const settings = async (runtime: PatchedSession, tag: string) => {
        const overlay = path.join(context.artifacts, `${tag}.overlay.bin`);
        await rpc(runtime.session, 'card.overlay', { path: overlay });
        const binary = patchkitBinary();
        const result = await exec(
          binary,
          ['store', 'dump', context.baseline('send').cardImage!, '--device', device, '--overlay', overlay, '--json'],
          { cwd: repoRoot(), timeout: 30_000 },
        );
        const dump = JSON.parse(result.stdout) as StoreDump;
        record(context.artifacts, `${tag}-store`, dump);
        const active = dump.settings.copies.find((copy) => copy.copy === dump.settings.active);
        expect(active, 'valid runtime settings copy').toBeDefined();
        return {
          overlay,
          seq: active!.seq,
          entries: new Map(active!.entries.map((entry) => [entry.key, entry.value_hex])),
        };
      };
      const pageTwo = async (runtime: PatchedSession) => {
        await tap(runtime.session, 'FX', 400);
        await tap(runtime.session, 'FX', 400);
      };

  // One `boot` per test: the first leg uses the fixture's own boot when it starts from the file's initial preset, later legs reload a clean baseline.
  const booter = (initial: Preset) => {
    let firstBoot = true;
    return async (tag: string, selectedNames: string[], preset: Preset) => {
      const runtime =
        firstBoot && preset === initial ? context.fixture.runtime : await context.fixture.reload(context.baseline(preset));
      firstBoot = false;
      const state = await inspectFxBoot(runtime, context.plan, selectedNames, context.artifacts, `${tag}-boot`);
      context.rememberEpoch(state.epoch);
      return { runtime, state };
    };
  };
  return { device, context, triggerFrame, base, pcm, locks, sym, chain, amounts, capacity, settings, pageTwo, booter };
}

/** Describe the track FX send page of every device: the optional base capture, then `register`, which adds the file's test. */
export function trackRouteSuite(
  scope: string,
  opts: { initial: Preset; presets: Preset[]; base: boolean },
  register: (t: TrackRoute) => void,
): void {
  for (const device of DEVICES) {
    describe(`${device.toUpperCase()} ${FIRMWARE[device]} track FX send page`, () => {
      const base = { hash: '', pcm: '' };
      const triggerFrame = triggerFor(device);
      if (opts.base) {
        describe('image without the page hook', () => {
          const context = withRouteFxSession(device, `track FX base ${scope}`, {
            image: 'track-base',
            initial: 'sine',
            presets: { sine: sineCard },
          });
          test('captures the original FX pixels and PCM', async () => {
            const { fixture, plan, profile, artifacts } = context;
            const runtime = fixture.runtime;
            const state = await inspectFxBoot(runtime, plan, [SINE], artifacts, 'base-boot');
            context.rememberEpoch(state.epoch);
            await selectSine(runtime, profile);
            await tap(runtime.session, 'FX', 400);
            base.hash = await shot(runtime.session, artifacts, 'base-fx1');
            await tap(runtime.session, 'NO', 300);
            base.pcm = path.join(artifacts, 'base.wav');
            await trackNote(runtime, base.pcm, triggerFrame);
          });
        });
      }
      describe('image with the page hook', () => {
        const context = withRouteFxSession(device, `track FX send pages ${scope}`, {
          image: 'track',
          initial: opts.initial,
          presets: Object.fromEntries(opts.presets.map((name) => [name, CARDS[name]])),
          // Page, kit and project legs are idle; PCM legs explicitly require an audible signal.
          requireAudible: false,
        });
        register(makeTrackRoute(device, context, triggerFrame, base));
      });
    });
  }
}
