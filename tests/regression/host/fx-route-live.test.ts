import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { afterEach, expect, test, vi } from 'vitest';
import type { Session } from '@digi/test-support';
import { DkSession } from '@digi/dk-session';
import type { PatchedSession } from '../support.ts';
import type { RuntimePlan } from '../runtime/runtime-plan.ts';

const fake = vi.hoisted(() => ({
  fixture: vi.fn(),
  prepare: vi.fn(),
  analysis: vi.fn(),
  watchEnv: vi.fn(),
  safety: vi.fn(),
  evidence: vi.fn(),
  exec: vi.fn(),
  ready: vi.fn(),
  noGuardHits: vi.fn(),
}));
vi.mock('node:child_process', () => ({
  execFile: Object.assign(vi.fn(), { [Symbol.for('nodejs.util.promisify.custom')]: fake.exec }),
}));
vi.mock('../support.ts', () => ({ withReusablePatchedSession: fake.fixture, expectNoGuardHits: fake.noGuardHits }));
vi.mock('../runtime/runtime-plan.ts', async (original) => ({
  ...(await original<object>()),
  prepareRuntimePlanOffline: fake.prepare,
}));
vi.mock('../runtime/write-safety.ts', () => ({ runtimeWatchEnv: fake.watchEnv, runtimeSafety: fake.safety }));
vi.mock('../runtime/fx-live.ts', async (original) => ({
  ...(await original<object>()),
  options: (device: string) => ({ device, firmware: device === 'dt2' ? '1.17' : '1.12' }),
  analysisJson: fake.analysis,
  evidence: fake.evidence,
}));
vi.mock('../runtime/ready.ts', async (original) => ({ ...(await original<object>()), expectRuntimeUi: fake.ready }));
import { withRouteFxSession } from '../runtime/fx-route-live.ts';
import {
  withPreparedFxSession,
  readTextRows,
  tap,
  inspectFxBoot,
  withPausedFxProtocol,
} from '../runtime/fx-session.ts';

const directories: string[] = [];
afterEach(() => {
  vi.restoreAllMocks();
  for (const directory of directories.splice(0)) fs.rmSync(directory, { recursive: true, force: true });
});

for (const image of ['routes', 'track', 'track-base', 'plain', 'kit'] as const) {
  test(`${image} cards keep the complete inventory and exactly the selected store paths`, async () => {
    vi.resetAllMocks();
    const plain = image === 'plain' || image === 'kit';
    const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'digi-route-host-'));
    directories.push(directory);
    const artifacts = path.join(directory, 'artifacts');
    fs.mkdirSync(artifacts);
    const mktemp = fs.mkdtempSync.bind(fs);
    vi.spyOn(fs, 'mkdtempSync').mockImplementation(((prefix: string) =>
      String(prefix).includes('digi-fx-') ? artifacts : mktemp(prefix)) as typeof fs.mkdtempSync);
    const exists = fs.existsSync.bind(fs);
    vi.spyOn(fs, 'existsSync').mockImplementation((file) => String(file).endsWith('/plusdrive') || exists(file));
    const count = image === 'routes' ? 5 : plain ? 1 : 4;
    const ids =
      image === 'routes'
        ? ['sine', 'dyn-fx-gain', 'dyn-fx-gain-send', 'dyn-fx-gain-sendonly', 'dyn-fx-gain-send-3']
        : plain
          ? ['sine']
          : ['sine', 'dyn-fx-gain-both', 'dyn-fx-gain-both-2', 'dyn-fx-gain-both-3'];
    const modulesRoot = path.join(directory, 'modules');
    fs.mkdirSync(path.join(modulesRoot, 'dt2'), { recursive: true });
    ids.forEach((id) => fs.writeFileSync(path.join(modulesRoot, 'dt2', `${id}.dkm`), id));
    vi.stubEnv('DIGI_RUNTIME_MODULES', modulesRoot);
    // The card cache and the card builder binary of this test (the cache hashes the binary).
    vi.stubEnv('DIGI_CARD_CACHE', path.join(directory, 'cards'));
    fs.writeFileSync(path.join(directory, 'plusdrive'), 'plusdrive');
    vi.stubEnv('DIGI_PLUSDRIVE', path.join(directory, 'plusdrive'));
    const plan: RuntimePlan = {
      base: { device: 'dt2', version: '1.17', syx_sha256: 'a'.repeat(64) },
      modules: ['core/runtime'],
      symbols: { 'core/runtime:DKRT_BOOT': 4096 },
      output: { syx_sha256: 'b'.repeat(64) },
    };
    const preparedImage = {
      syx: '/prepared.syx',
      baseSha256: plan.base.syx_sha256,
      syxSha256: plan.output!.syx_sha256,
      modules: plan.modules,
    };
    fake.prepare.mockResolvedValue({ plan, preparedImage, planFile: '/prepared.plan.json', syx: '/prepared.syx' });
    fake.analysis.mockImplementation(async (_module: string, args: string[]) => {
      if (args[0] === 'seed-card') settings.push(JSON.parse(fs.readFileSync(args[5]!, 'utf8')));
      return args[0] === 'profile' ? { fx_slots: 3, send_slots: 2, track_type_word: 1234 } : {};
    });
    fake.watchEnv.mockReturnValue({ DIGI_WRITE_WATCH: '0x1000-0x2000' });
    fake.safety.mockReturnValue({ pass: true });
    // The card is built in a work directory that is gone afterwards: read what the build is given when it runs.
    const seeds: { modules: string[]; contents: string[]; selection: string }[] = [];
    const settings: unknown[] = [];
    fake.exec.mockImplementation(async (_binary: string, args: string[]) => {
      const seed = args[1]!;
      seeds.push({
        modules: fs.readdirSync(path.join(seed, 'digikit/modules')),
        contents: fs
          .readdirSync(path.join(seed, 'digikit/modules'))
          .map((name) => fs.readFileSync(path.join(seed, 'digikit/modules', name), 'utf8')),
        selection: fs.readFileSync(path.join(seed, 'digikit/selection.txt'), 'utf8'),
      });
      return { stdout: '', stderr: '' };
    });
    const presets = {
      dry: { selection: ['modules/m0.dkm'] },
      wet: {
        selection: plain ? ['modules/m0.dkm'] : ['modules/m0.dkm', 'modules/m1.dkm'],
        settings: [['fx/route/test/effect', '01']] as [string, string][],
      },
    };
    const result = plain
      ? withPreparedFxSession('dt2', 'host plain', {
          image: {
            name: image,
            modules: ['core/runtime', 'core/dynreg', 'core/dyndsp'],
            inventory: ['test/sine'],
            symbols: ['DKRT_BOOT'],
            policy: image === 'kit' ? 'fx-kit' : 'default',
          },
          initial: 'dry',
          presets,
          requireAudible: false,
        })
      : withRouteFxSession('dt2', 'host route', { image, initial: 'dry', presets, requireAudible: false });
    const [opts, lifecycle] = fake.fixture.mock.calls[0]!;
    try {
      await opts.env();
      const session = { runDir: directory, status: vi.fn(async () => ({ epoch: 1 })) } as unknown as Session;
      await lifecycle.prepareBaseline(session);
      const modules = Array.from({ length: count }, (_, i) => `m${i}.dkm`);
      expect(seeds.map((s) => s.modules)).toEqual([modules, modules]);
      expect(seeds[1]!.selection).toBe(plain ? 'modules/m0.dkm\n' : 'modules/m0.dkm\nmodules/m1.dkm\n');
      expect(settings).toEqual([[{ key: 'fx/route/test/effect', value_hex: '01' }]]);
      expect(result.baseline('wet').preparedImage).toBe(preparedImage);
      expect(() => result.baseline('absent')).toThrow('unknown');
      expect(result.slots).toBe(3);
      expect(result.sendSlots).toBe(2);
      expect(result.profile.track_type_word).toBe(1234);
      expect(fake.prepare).toHaveBeenCalledTimes(1);
      const requestedSymbols = fake.prepare.mock.calls[0]![3];
      const requestedRoots = fake.prepare.mock.calls[0]![4].modules;
      const commonSymbols = ['DKRT_BOOT', 'DKRT_ASSIGN', 'fx.rows', '_dl_area'];
      const roots = ['core/runtime', 'core/dynreg', 'core/dyndsp'];
      const expectedSymbols = plain
        ? ['DKRT_BOOT']
        : image === 'track-base'
          ? [...commonSymbols, 'dynreg_info']
          : image === 'track'
            ? [
                ...commonSymbols,
                'dynreg_info',
                'fxpages_info',
                'fxp_send_row',
                'fxp_send_count',
                'fxp_nload',
                'DKRT_FXS',
              ]
            : [
                ...commonSymbols,
                'DK_FXR_PEND',
                'DK_FXR_SET',
                'DK_FXR_FLUSH',
                'DK_POPUP',
                'DK_POPUP_TEXT',
                'DK_POPUPS',
                'DK_APPLIES',
                'DK_PANEL',
                'DK_PANEL_N',
                'DK_ROUTE_N',
                'DK_ROUTE_ITEM',
                'fxp_send_count',
                'DKRT_FXS',
                'fxpages_info',
              ];
      expect(requestedSymbols).toEqual(expectedSymbols);
      expect(requestedRoots).toEqual([
        ...roots,
        ...(plain ? [] : ['core/dynfx']),
        ...(plain || image === 'track-base' ? [] : ['core/fxpages']),
        ...(image === 'routes' ? ['core/digikit-menu'] : []),
      ]);
      expect(seeds[0]!.contents).toEqual(ids);
      await lifecycle.finishCase({ session });
      expect(fake.evidence).toHaveBeenCalledWith({ session }, artifacts, 'case-1', [], false);
      await lifecycle.afterClose();
      const safetyCall = fake.safety.mock.calls[0]!;
      expect(safetyCall[4]).toBe(image === 'kit' ? 'fx-kit' : plain ? 'default' : 'fx-routes');
      if (plain) {
        fake.prepare.mockResolvedValueOnce({
          plan: { ...plan, symbols: {} },
          preparedImage,
          planFile: '/prepared.plan.json',
          syx: '/prepared.syx',
        });
        await expect(opts.env()).rejects.toThrow('symbol DKRT_BOOT: 0 addresses');
      }
      if (image === 'routes') {
        for (const failing of ['runtime-check', 'render-shots']) {
          const observed: string[] = [];
          fake.safety.mockImplementation(() => {
            observed.push('runtime-check');
            if (failing === 'runtime-check') throw new Error('runtime-check failed');
            return { pass: true };
          });
          fake.analysis.mockImplementation(async (_module: string, args: string[]) => {
            observed.push(args[0]!);
            if (args[0] === failing) throw new Error(`${failing} failed`);
            return { pass: true };
          });
          await expect(lifecycle.afterClose()).rejects.toThrow(`${failing} failed`);
          expect(observed).toContain('runtime-check');
          expect(observed).toContain('render-shots');
        }
      }
    } finally {
      vi.unstubAllEnvs();
    }
  });
}

test('route diagnostic text decodes big endian words and panel chords release on failure', async () => {
  const session = {
    client: {
      memPeek: vi.fn(async () => 0x41420000),
      inputButton: vi.fn(),
      inputTap: vi.fn().mockRejectedValue(new Error('panel failure')),
      step: vi.fn(),
    },
  } as unknown as Session;
  expect(await readTextRows(session, 4096, 2, 4)).toEqual(['AB', 'AB']);
  await expect(tap(session, 'YES', 0, 'FUNC')).rejects.toThrow('panel failure');
  expect(session.client.inputButton).toHaveBeenLastCalledWith(expect.any(Number), false);
});

for (const { afterEpoch, loaderResult } of [
  { afterEpoch: undefined, loaderResult: 0 },
  { afterEpoch: 2, loaderResult: 0 },
  { afterEpoch: undefined, loaderResult: -34 },
]) {
  test(`route boot drains input and returns paused with ${afterEpoch === undefined ? 'strict fresh' : 'explicit restart'} readiness and loader result ${loaderResult}`, async () => {
    const artifacts = fs.mkdtempSync(path.join(os.tmpdir(), 'digi-route-inspect-'));
    directories.push(artifacts);
    const order: string[] = [];
    let mode = 'paused';
    let observations = 0;
    let pcmStarted = false;
    const expectedNames = loaderResult === 0 ? ['test/sine'] : [];
    const fields = [0x444b4254, 1, 0, 0, 1, 0, 0, 1, expectedNames.length, 1, loaderResult >>> 0];
    const plan: RuntimePlan = {
      base: { device: 'dt2', version: '1.17', syx_sha256: 'a'.repeat(64) },
      modules: ['core/runtime'],
      symbols: { 'core/runtime:DKRT_BOOT': 4096 },
      output: { syx_sha256: 'b'.repeat(64) },
    };
    fake.ready.mockImplementation(async () => {
      order.push('ready');
      expect(mode).toBe('free');
    });
    const session = {
      expectNoFault: vi.fn(),
      options: { device: 'dt2', firmware: '1.17' },
      loadResult: { syx_sha256: plan.output!.syx_sha256 },
      preparedRuntimeImage: {
        syx: '/prepared.syx',
        baseSha256: plan.base.syx_sha256,
        syxSha256: plan.output!.syx_sha256,
        modules: plan.modules,
      },
      client: {
        call: vi.fn(async (method: string, params: Record<string, unknown>) => {
          if (method === 'run.mode') {
            mode = String(params.mode);
            order.push(mode);
          } else if (method === 'pcm.tap') {
            order.push(`pcm:${params.action}`);
            if (params.action === 'start') pcmStarted = true;
            if (params.action === 'report') {
              if (!pcmStarted) throw new Error('pcm.tap was not started');
              return { non_finite: 0, peak: 0 };
            }
          }
          return {};
        }),
        guardReport: vi.fn(async () => ({ total: 0, hits: [] })),
        memPeek: vi.fn(async (address: number) => fields[(address - 4096) / 4]),
      },
      status: vi.fn(async () => {
        const pending = observations++ === 0 ? 1 : 0;
        order.push(pending ? 'pending' : 'drained');
        if (pending) expect(mode).toBe('free');
        return { epoch: 3, mode, cf: { input_pending: pending }, scheduled_inputs: pending };
      }),
    } as unknown as Session;
    const dk = {
      modules: vi.fn(async () => {
        expect(mode).toBe('free');
        return expectedNames.map((name) => ({ name }));
      }),
      capacity: vi.fn(async () => {
        expect(mode).toBe('free');
        order.push('capacity');
        return {};
      }),
    };
    const inspected = await inspectFxBoot(
      { session, dk } as unknown as PatchedSession,
      plan,
      expectedNames,
      artifacts,
      'boot',
      loaderResult === 0 ? undefined : [loaderResult],
      afterEpoch === undefined ? undefined : { afterEpoch },
    );
    expect(fake.ready).toHaveBeenLastCalledWith(
      session,
      plan,
      artifacts,
      afterEpoch === undefined ? {} : { afterEpoch },
    );
    // The drain wait reads the status clock between polls: count a run of the same step once.
    expect(order.filter((step, i) => step !== order[i - 1])).toEqual([
      'pcm:start',
      'free',
      'ready',
      'capacity',
      'pending',
      'drained',
      'paused',
      'drained',
    ]);
    expect(mode).toBe('paused');
    expect(inspected.epoch).toBe(3);
    expect(inspected.results).toEqual([loaderResult]);
    const { evidence } = await vi.importActual<typeof import('../runtime/fx-live.ts')>('../runtime/fx-live.ts');
    await expect(evidence({ session } as PatchedSession, artifacts, 'closing', [], false)).resolves.toBeUndefined();
    expect(order.indexOf('pcm:start')).toBeLessThan(order.indexOf('ready'));
    expect(order.indexOf('pcm:report')).toBeGreaterThan(order.indexOf('paused'));
    expect(session.expectNoFault).toHaveBeenCalledWith('closing');
  });
}

test('paused route protocol owns its stepped transport and cancels requests on failure', async () => {
  const cancel = vi.spyOn(DkSession.prototype, 'cancelPending');
  const calls: string[] = [];
  let stepped = false;
  let mode = 'paused';
  const session = {
    status: vi.fn(async () => ({ mode })),
    client: {
      call: vi.fn(async (method: string, params?: Record<string, unknown>) => {
        calls.push(method);
        if (method === 'step') {
          expect(params?.instructions).toBe(400_000);
          stepped = true;
          await new Promise((resolve) => setTimeout(resolve, 1));
        }
        return { hex: '' };
      }),
    },
  } as unknown as Session;
  const runtime = { session } as PatchedSession;
  let pending: Promise<unknown> | undefined;
  await expect(
    withPausedFxProtocol(runtime, async (dk) => {
      pending = expect(dk.request(1)).rejects.toMatchObject({ kind: 'cancelled' });
      await vi.waitFor(() => expect(stepped).toBe(true));
      throw new Error('inspection failed');
    }),
  ).rejects.toThrow('inspection failed');
  await pending;
  expect(cancel).toHaveBeenCalledTimes(1);
  expect(calls).not.toContain('run.mode');
  mode = 'free';
  const operation = vi.fn();
  await expect(withPausedFxProtocol(runtime, operation)).rejects.toThrow('require a paused runtime');
  expect(operation).not.toHaveBeenCalled();
});

test('prepared FX fixtures require an explicit safety policy', () => {
  const config = {
    image: { name: 'plain', modules: ['core/runtime'], inventory: [], symbols: ['DKRT_BOOT'] },
    initial: 'empty',
    presets: { empty: { selection: [] } },
  };
  expect(() =>
    withPreparedFxSession('dt2', 'missing policy', config as unknown as Parameters<typeof withPreparedFxSession>[2]),
  ).toThrow('explicit safety policy');
});
