// The Profile pane's recorder (src/profile-backend.ts) over a fake session, a fake controller rpc and a fake library: what it reads, what Start,
// Stop, Reset and Export do on the emulator and on a device.
import assert from 'node:assert/strict';
import { describe, test } from 'vitest';
import { DkError } from '@digi/dk-session';
import { CAP, LOG_KIND } from '@digi/dk-session/gen_proto';
import type { EmuRpc } from '@digi/workbench-core/host/host.ts';
import { ownerName } from '@digi/workbench-core/profile/profile-core.ts';
import type { LibraryModule } from '@digi/workbench-core/modules/dktransfer.ts';
import { createRecorder, isDebugMissing, type ProfileSession } from '../src/profile-backend.ts';

const HZ = 132_000_000;
const NOW = new Date('2026-11-15T12:34:56Z');

interface FakeState {
  caps: number | null;
  running: boolean;
  elapsed: bigint;
  missing: boolean;
  calls: string[];
  health?: { avg: number; max: number };
  polls?: ({ owner: number | 'runtime'; fn: number } | undefined)[];
}

function fakeSession(st: FakeState): ProfileSession {
  const span = (
    row: number,
    kind: 'module' | 'phase' | 'poll',
    index: number,
    calls: number,
    sum: bigint,
    max: number,
  ) => ({
    row,
    kind,
    index,
    calls,
    sumTicks: sum,
    maxTicks: max,
  });
  return {
    info: async () => ({ port: 'dn2-1.12', caps: st.caps }),
    modules: async () => [
      { index: 0, name: 'test/saw' },
      { index: 1, name: 'someone/mystery' },
    ],
    capacity: async () => ({ DSP_COST: { limit: 92_796, used: 0, largest: 0 } }),
    profileState: async (op: 'get' | 'start' | 'stop' | 'reset') => {
      st.calls.push(`state:${op}`);
      if (op === 'start') st.running = true;
      if (op === 'stop') st.running = false;
      if (op === 'reset') st.elapsed = 0n;
      return { running: st.running, hz: HZ, rows: 15, elapsedTicks: st.elapsed };
    },
    profileAll: async () => {
      if (st.missing) throw new DkError({ error: 'unknown command', kind: 'device', status: 'E_PROTO' });
      return {
        running: st.running,
        hz: HZ,
        elapsedTicks: st.elapsed,
        spans: [span(0, 'module', 0, 10, 1000n, 300), span(8, 'phase', 0, 5, 50n, 20), span(11, 'poll', 0, 0, 0n, 0)],
        polls: st.polls ?? [],
      };
    },
    logPage: async () => {
      const ev = (seq: number, kind: number, args: number[]) => ({ seq, time: 0, kind, flags: 0, args });
      const events = st.health
        ? [ev(1, LOG_KIND.HEALTH, [4 << 16, st.health.avg * 10, st.health.max * 10, 0, 0]), ev(2, 1, [0, 0, 0, 0, 0])]
        : [ev(2, 1, [0, 0, 0, 0, 0])];
      return [1, 2, events] as [number, number, typeof events];
    },
    peek: async () => new Uint8Array(),
  } as unknown as ProfileSession;
}

const library = async (): Promise<LibraryModule[]> =>
  [{ name: 'test/saw', group: 'Machines' }] as unknown as LibraryModule[];

function fakeRpc(calls: string[], answers: Record<string, unknown>, fail: string[] = []): EmuRpc {
  return {
    call: async <T>(method: string, params?: Record<string, unknown>) => {
      calls.push(params ? `${method} ${JSON.stringify(params)}` : method);
      if (fail.includes(method)) throw new Error('built without it');
      const a = answers[method];
      return (Array.isArray(a) ? a.shift() : a) as T;
    },
  };
}

const sharcAnswer = {
  clock: { source: 'emu.sharc.insn', unit: 'insn', sampling: 's' },
  blocks: 100,
  icount: 1,
  idle_skipped: 0,
  counted: { fast: 1, block: 0, interpreted: 0 },
  raw: [],
};
const cfAnswer = { pc_samples: [['100', 3]], paths: [] };

const state = (over: Partial<FakeState> = {}): FakeState => ({
  caps: 1 << CAP.PEEK,
  running: false,
  elapsed: 0n,
  missing: false,
  calls: [],
  ...over,
});

describe('read', () => {
  test('a device without the PEEK capability, or one that refuses PROFILE_STATE, has no debug module', async () => {
    for (const st of [state({ caps: 0 }), state({ missing: true })]) {
      const r = await createRecorder({
        session: async () => fakeSession(st),
        loadLibrary: library,
        now: () => NOW,
      }).read();
      assert.equal(r.noDebug, true);
    }
    assert.equal(isDebugMissing(new DkError({ error: 'x', kind: 'device', status: 'E_PROTO' })), true);
    assert.equal(isDebugMissing(new DkError({ error: 'x', kind: 'device', status: 'E_FULL' })), false);
    assert.equal(isDebugMissing(new Error('x')), false);
  });

  test('the spans and the modules, typed by the library; a module the library lacks is other', async () => {
    const r = await createRecorder({
      session: async () => fakeSession(state({ running: true, elapsed: 5000n })),
      loadLibrary: library,
      now: () => NOW,
    }).read();
    assert.ok(!r.noDebug);
    if (r.noDebug) return;
    assert.equal(r.emulator, false);
    assert.equal(r.snap.running, true);
    assert.equal(r.snap.elapsedTicks, 5000n);
    assert.deepEqual(r.modules, [
      { index: 0, name: 'test/saw', kind: 'machine' },
      { index: 1, name: 'someone/mystery', kind: 'other' },
    ]);
  });

  test('poll slots are named by their module and, when the address map names the function, by it', async () => {
    const asked: number[] = [];
    const rec = createRecorder({
      session: async () =>
        fakeSession(
          state({
            running: true,
            elapsed: 5000n,
            polls: [{ owner: 1, fn: 0x47e10040 }, undefined, { owner: 'runtime', fn: 0x40101000 }],
          }),
        ),
      loadLibrary: library,
      prof: async () =>
        ({
          addrLookup: (req: { addrs: { addr: number }[] }) => {
            asked.push(...req.addrs.map((a) => a.addr));
            return {
              unloaded: [],
              results: req.addrs.map((a) => ({ text: a.addr === 0x47e10040 ? 'poll_fn' : null })),
            };
          },
        }) as never,
      names: { inputs: () => ({ maps: [], loads: { modules: [] } }), refresh: async () => {} },
      now: () => NOW,
    });
    const r = await rec.read();
    assert.ok(!r.noDebug);
    if (r.noDebug) return;
    assert.deepEqual(asked, [0x47e10040, 0x40101000], 'one lookup of the used slots');
    assert.deepEqual(r.snap.polls, [
      { owner: 1, fn: 0x47e10040, name: 'poll_fn' },
      undefined,
      { owner: 'runtime', fn: 0x40101000 },
    ]);
    assert.equal(ownerName({ kind: 'poll', index: 0 }, r.modules, r.snap.polls), 'someone/mystery poll: poll_fn');
    assert.equal(ownerName({ kind: 'poll', index: 2 }, r.modules, r.snap.polls), 'runtime poll');
  });

  test('a library that cannot be read leaves every module other', async () => {
    const r = await createRecorder({
      session: async () => fakeSession(state({ elapsed: 1n })),
      loadLibrary: async () => {
        throw new Error('no folder');
      },
    }).read();
    assert.ok(!r.noDebug && r.modules.every((m) => m.kind === 'other'));
  });

  test('a device reads the whole-block DSP load from the newest HEALTH event, an emulator does not', async () => {
    const dev = await createRecorder({
      session: async () => fakeSession(state({ elapsed: 1n, health: { avg: 21.3, max: 34 } })),
      loadLibrary: library,
    }).read();
    assert.ok(!dev.noDebug);
    if (!dev.noDebug) assert.deepEqual(dev.deviceDsp, { avgPct: 21.3, maxPct: 34 });
    const none = await createRecorder({ session: async () => fakeSession(state()), loadLibrary: library }).read();
    assert.ok(!none.noDebug && none.deviceDsp === undefined);
  });
});

describe('the emulator', () => {
  test('Start resets the counters, starts the DSP and ColdFire profiles, then starts the spans', async () => {
    const st = state();
    const rpcCalls: string[] = [];
    const rec = createRecorder({
      session: async () => fakeSession(st),
      rpc: fakeRpc(rpcCalls, { 'cf.profile': cfAnswer, 'sharc.profile': sharcAnswer }),
      loadLibrary: library,
    });
    await rec.start();
    assert.deepEqual(st.calls, ['state:reset', 'state:start']);
    assert.deepEqual(rpcCalls, ['cf.profile', 'sharc.profile {"enable":true}']);
  });

  test('the DSP view appears while recording, with the device headroom, and names no module without maps', async () => {
    const st = state({ elapsed: 100n });
    const rec = createRecorder({
      session: async () => fakeSession(st),
      rpc: fakeRpc([], { 'cf.profile': cfAnswer, 'sharc.profile': sharcAnswer }),
      loadLibrary: library,
    });
    await rec.start();
    st.elapsed = 100n;
    const r = await rec.read();
    assert.ok(!r.noDebug);
    if (r.noDebug) return;
    assert.equal(r.emulator, true);
    assert.equal(r.dsp?.headroom, 92_796);
    assert.equal(r.dsp?.blocks, 100);
    assert.deepEqual(r.dsp?.modules, []);
  });

  test('Stop keeps the instruction profiles for Export and switches the DSP count off', async () => {
    const st = state({ elapsed: 5000n });
    const rpcCalls: string[] = [];
    const rec = createRecorder({
      session: async () => fakeSession(st),
      rpc: fakeRpc(rpcCalls, {
        // the baseline at Start, then the answer at Stop
        'cf.profile': [cfAnswer, { pc_samples: [['100', 10]], paths: [] }],
        'sharc.profile': sharcAnswer,
      }),
      loadLibrary: library,
      now: () => NOW,
    });
    await rec.start();
    st.elapsed = 5000n;
    rpcCalls.length = 0;
    await rec.stop();
    assert.deepEqual(rpcCalls, ['cf.profile', 'sharc.profile', 'sharc.profile {"enable":false}']);
    rpcCalls.length = 0;
    const f = await rec.exportFile();
    assert.deepEqual(rpcCalls, [], 'the stopped recording is exported from what Stop kept');
    const doc = JSON.parse(f.text);
    assert.equal(f.name, 'emu-20261115T123456.dkprof.json');
    assert.equal(doc.schema, 'dk.profile/1');
    assert.equal(doc.run.target, 'dn2');
    assert.equal(doc.span_elapsed_ticks, 5000);
    assert.equal(doc.span_clock_hz, HZ);
    assert.deepEqual(
      doc.raw.map((r: { cpu: string; count: number }) => [r.cpu, r.count]),
      [['cf', 7]],
      'cf.profile is the difference to the Start baseline',
    );
    assert.ok(doc.metrics.some((m: { name: string }) => m.name === 'sharc.blocks'));
    assert.deepEqual(
      doc.spans.map((s: { name: string; owner: number }) => [s.owner, s.name]),
      [
        [0, 'test/saw'],
        [0x100, 'early'],
        [0x200, 'poll 0'],
      ],
    );
  });

  test('a daemon built without the profile features still records the spans and says so', async () => {
    const st = state({ running: true, elapsed: 10n });
    const rec = createRecorder({
      session: async () => fakeSession(st),
      rpc: fakeRpc([], {}, ['cf.profile', 'sharc.profile']),
      loadLibrary: library,
    });
    await rec.start();
    assert.deepEqual(st.calls, ['state:reset', 'state:start']);
    const r = await rec.read();
    assert.ok(!r.noDebug);
    if (!r.noDebug) assert.match(r.note ?? '', /cf\.profile and sharc\.profile not available/);
  });

  test('Reset zeroes the span counters and counts the instruction profiles from now', async () => {
    const st = state();
    const rpcCalls: string[] = [];
    const rec = createRecorder({
      session: async () => fakeSession(st),
      rpc: fakeRpc(rpcCalls, { 'cf.profile': cfAnswer, 'sharc.profile': sharcAnswer }),
      loadLibrary: library,
    });
    await rec.start();
    st.calls.length = 0;
    rpcCalls.length = 0;
    await rec.reset();
    assert.deepEqual(st.calls, ['state:reset']);
    assert.deepEqual(rpcCalls, ['cf.profile', 'sharc.profile {"enable":true}']);
  });
});

describe('a device', () => {
  test('Start and Stop are the spans only, and Export writes the spans', async () => {
    const st = state({ elapsed: 777n });
    const rec = createRecorder({ session: async () => fakeSession(st), loadLibrary: library, now: () => NOW });
    await rec.start();
    st.elapsed = 777n; // the window grows while it records
    await rec.stop();
    assert.deepEqual(st.calls, ['state:reset', 'state:start', 'state:stop']);
    const doc = JSON.parse((await rec.exportFile()).text);
    assert.equal(doc.clock.source, 'runtime.span');
    assert.deepEqual(doc.raw, []);
    assert.equal(doc.span_elapsed_ticks, 777);
    assert.match(doc.run.id, /^dev-/);
  });
});
