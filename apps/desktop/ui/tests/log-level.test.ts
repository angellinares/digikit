// The Level menu of the Log drawer (apps/desktop/ui/src/log-level-core.ts and the levels calls of log-follow.ts) over a fake runtime that keeps the
// levels as the real one does.
import assert from 'node:assert/strict';
import { createRoot } from 'solid-js';
import { test } from 'vitest';
import type { LogConfig } from '@digi/dk-session';
import { CAP } from '@digi/dk-session/gen_proto';
import { createLogFollow } from '../src/log-follow.ts';
import type { FollowSession } from '../src/log-follow-core.ts';
import {
  USE_DEFAULT,
  changeLevel,
  levelChoices,
  levelLabel,
  levelOrigin,
  levelsOff,
  readLevels,
  requestFor,
} from '../src/log-level-core.ts';

const off = (): number[] => Array<number>(8).fill(USE_DEFAULT);

class Runtime implements FollowSession {
  caps: number | null = (1 << CAP.LOG) | (1 << CAP.LOGCFG);
  cfg: LogConfig = { level: 2, modules: off(), debug: true };
  sets: [number, number[]][] = [];
  async info() {
    return { caps: this.caps };
  }
  async logPage(): Promise<[number, number, never[]]> {
    return [0, 0, []];
  }
  async logConfig() {
    return { ...this.cfg };
  }
  async setLogConfig(level: number, modules: number[]) {
    this.sets.push([level, modules]);
    this.cfg = { ...this.cfg, level, modules };
    return { ...this.cfg };
  }
  async modules() {
    return [
      { index: 1, name: 'digikit/remote' },
      { index: 0, name: 'freqshift' },
    ];
  }
  async fault() {
    return null;
  }
}

test('the levels of the origin: the default, the loaded modules in record order', async () => {
  const rt = new Runtime();
  rt.cfg.modules[1] = 4;
  const v = await readLevels(rt);
  assert.deepEqual(v, {
    level: 2,
    debug: true,
    modules: [
      { index: 0, name: 'freqshift', level: USE_DEFAULT },
      { index: 1, name: 'digikit/remote', level: 4 },
    ],
  });
});

test('an origin without LOGCFG has no levels, and a change is refused without a set', async () => {
  const rt = new Runtime();
  rt.caps = 1 << CAP.LOG;
  assert.equal(await readLevels(rt), null);
  await assert.rejects(changeLevel(rt, { what: 'default', level: 3 }), /debug module/);
  assert.equal(rt.sets.length, 0);
});

test('a change keeps the other records and shows the state read back', async () => {
  const rt = new Runtime();
  rt.cfg.modules[1] = 4;
  const v = await changeLevel(rt, { what: 'module', index: 0, level: 0 });
  assert.deepEqual(rt.sets.at(-1), [2, [0, 4, ...Array(6).fill(USE_DEFAULT)]]);
  assert.deepEqual(
    v.modules.map((m) => m.level),
    [0, 4],
  );
  const d = await changeLevel(rt, { what: 'default', level: 4 });
  assert.deepEqual(rt.sets.at(-1), [4, [0, 4, ...Array(6).fill(USE_DEFAULT)]]);
  assert.equal(d.level, 4);
  // a record set back to the default
  await changeLevel(rt, { what: 'module', index: 1, level: USE_DEFAULT });
  assert.deepEqual(rt.sets.at(-1)![1], [0, ...Array(7).fill(USE_DEFAULT)]);
});

test('requestFor always sends 8 records, also for a short or empty config', () => {
  assert.deepEqual(requestFor({ level: 1, modules: [], debug: false }, { what: 'default', level: 3 }), {
    level: 3,
    modules: off(),
  });
});

test('the Level button text and the choices of a row', () => {
  assert.equal(levelLabel(undefined), 'Level');
  assert.equal(levelLabel(2, off()), 'Level: info');
  assert.equal(levelLabel(4, [0xff, 1, 0, ...Array(5).fill(0xff)]), 'Level: trace +2');
  assert.deepEqual(
    levelChoices('default').map((c) => c.label),
    ['error', 'warn', 'info', 'debug', 'trace'],
  );
  assert.deepEqual(
    levelChoices('module').map((c) => c.label),
    ['default', 'error', 'warn', 'info', 'debug', 'trace'],
  );
});

test('the menu acts on the origin chosen in Source; All and the app sources only when one origin is followed', () => {
  const both = { emulator: true, device: true };
  assert.equal(levelOrigin('device:mod:freqshift', both), 'device');
  assert.equal(levelOrigin('emulator:runtime', both), 'emulator');
  assert.equal(levelOrigin('emulator', both), 'emulator');
  assert.equal(levelOrigin('all', both), undefined);
  assert.equal(levelOrigin('all', { emulator: false, device: true }), 'device');
  assert.equal(levelOrigin('reload', { emulator: true, device: false }), 'emulator');
  assert.equal(levelOrigin('all', { emulator: false, device: false }), undefined);
});

test('why the controls are off', () => {
  assert.match(levelsOff({ origin: undefined, live: false, supported: undefined }), /Choose Emulator or Device/);
  assert.match(levelsOff({ origin: 'device', live: false, supported: undefined }), /not followed/);
  assert.match(levelsOff({ origin: 'device', live: true, supported: false }), /no log level control/);
  assert.equal(levelsOff({ origin: 'device', live: true, supported: true }), '');
  assert.equal(levelsOff({ origin: 'device', live: true, supported: undefined }), '');
});

test('the follow object sets a level on the origin asked and keeps the state it read back', async () => {
  const emulator = new Runtime();
  const device = new Runtime();
  await createRoot(async (dispose) => {
    const follow = createLogFollow({
      follow: () => false,
      flashing: () => false,
      emulator: { running: () => true, session: async () => emulator },
      device: { port: () => 'Digitakt', session: async () => device, present: async () => true },
    });
    assert.equal(follow.config().device, undefined);
    await follow.setLevel('device', { what: 'default', level: 4 });
    assert.equal(device.cfg.level, 4);
    assert.equal(emulator.cfg.level, 2);
    assert.equal(follow.config().device?.level, 4);
    assert.equal(follow.logcfg().device, true);
    device.caps = 1 << CAP.LOG;
    assert.equal(await follow.levels('device'), null);
    assert.equal(follow.logcfg().device, false);
    dispose();
  });
});
